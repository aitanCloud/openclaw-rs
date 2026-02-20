use anyhow::Result;
use chrono::{Datelike, NaiveDateTime, Timelike, Utc};
use openclaw_agent::llm::fallback::FallbackProvider;
use openclaw_agent::llm::LlmProvider;
use openclaw_agent::runtime::{AgentTurnConfig, AgentTurnResult};
use openclaw_agent::tools::ToolRegistry;
use openclaw_agent::workspace;
use openclaw_core::cron::{CronJob, CronSchedule};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::GatewayConfig;
use crate::telegram::TelegramBot;

/// Tracks last-run times for each job to prevent double-firing.
struct CronState {
    last_run: std::collections::HashMap<String, i64>,
}

/// Start the cron executor background task.
/// Checks jobs every 30 seconds and fires any that are due.
pub fn spawn_cron_executor(config: Arc<GatewayConfig>, bot: Arc<TelegramBot>) {
    let state = Arc::new(Mutex::new(CronState {
        last_run: std::collections::HashMap::new(),
    }));

    tokio::spawn(async move {
        info!("Cron executor started");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;

            if let Err(e) = tick(&config, &bot, &state).await {
                error!("Cron tick error: {}", e);
            }
        }
    });
}

async fn tick(
    config: &GatewayConfig,
    bot: &TelegramBot,
    state: &Arc<Mutex<CronState>>,
) -> Result<()> {
    let cron_path = cron_jobs_path();
    if !cron_path.exists() {
        return Ok(());
    }

    let cron_file = openclaw_core::cron::load_cron_jobs(&cron_path)?;
    let now = Utc::now();
    let now_ms = now.timestamp_millis();

    for job in &cron_file.jobs {
        if !job.enabled {
            continue;
        }

        if job.payload.kind != "agentTurn" {
            continue;
        }

        if !should_fire(job, now_ms, state).await {
            continue;
        }

        info!("Cron job firing: {} ({})", job.name, job.id);

        // Mark as fired
        {
            let mut s = state.lock().await;
            s.last_run.insert(job.id.clone(), now_ms);
        }

        // Run the agent turn
        let config = config.clone();
        let bot_clone = Arc::new(TelegramBot::new(&config.telegram.bot_token));
        let job_name = job.name.clone();
        let job_id = job.id.clone();
        let message = job.payload.message.clone();
        let delivery_chat_id = resolve_delivery_chat(&config);

        tokio::spawn(async move {
            match run_cron_agent_turn(&config, &message).await {
                Ok(result) => {
                    info!(
                        "Cron job '{}' completed: {} rounds, {} tool calls, {}ms",
                        job_name, result.total_rounds, result.tool_calls_made, result.elapsed_ms
                    );

                    // Deliver result to Telegram if we have a chat ID
                    if let Some(chat_id) = delivery_chat_id {
                        let header = format!("⏰ *Cron: {}*\n\n", job_name);
                        let response = if result.response.len() > 3800 {
                            format!("{}{}...", header, &result.response[..3800])
                        } else {
                            format!("{}{}", header, result.response)
                        };

                        if let Err(e) = bot_clone.send_message(chat_id, &response).await {
                            error!("Failed to deliver cron result for '{}': {}", job_name, e);
                        }
                    }

                    // Update job state in jobs.json
                    if let Err(e) = update_job_state(&job_id, result.elapsed_ms, "ok") {
                        warn!("Failed to update cron job state: {}", e);
                    }
                }
                Err(e) => {
                    error!("Cron job '{}' failed: {}", job_name, e);

                    if let Some(chat_id) = delivery_chat_id {
                        let msg = format!("⏰ *Cron: {}*\n\n❌ Error: {}", job_name, e);
                        let _ = bot_clone.send_message(chat_id, &msg).await;
                    }

                    if let Err(e2) = update_job_state(&job_id, 0, "error") {
                        warn!("Failed to update cron job state: {}", e2);
                    }
                }
            }
        });
    }

    Ok(())
}

async fn should_fire(job: &CronJob, now_ms: i64, state: &Arc<Mutex<CronState>>) -> bool {
    let s = state.lock().await;

    // Don't fire if we ran this job in the last 55 seconds (prevent double-fire)
    if let Some(&last) = s.last_run.get(&job.id) {
        if now_ms - last < 55_000 {
            return false;
        }
    }

    match &job.schedule {
        CronSchedule::Cron { expr, tz } => {
            let now_utc = Utc::now();

            // Apply timezone offset if specified
            let now_local = if let Some(tz_str) = tz {
                apply_timezone_offset(&now_utc, tz_str)
            } else {
                now_utc.naive_utc()
            };

            matches_cron_expr(expr, &now_local)
        }
        CronSchedule::Every { every_ms, anchor_ms } => {
            let anchor = anchor_ms.unwrap_or(0) as i64;
            let interval = *every_ms as i64;
            if interval == 0 {
                return false;
            }
            let elapsed = now_ms - anchor;
            let current_period = elapsed / interval;
            let period_start = anchor + current_period * interval;

            // Fire if we're within the first 45 seconds of a period
            // and haven't fired in this period yet
            let into_period = now_ms - period_start;
            if into_period > 45_000 {
                return false;
            }

            // Check we haven't already fired in this period
            if let Some(&last) = s.last_run.get(&job.id) {
                if last >= period_start {
                    return false;
                }
            }

            true
        }
    }
}

/// Parse a 5-field cron expression and check if the given time matches.
/// Fields: minute hour day-of-month month day-of-week
fn matches_cron_expr(expr: &str, now: &NaiveDateTime) -> bool {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        warn!("Invalid cron expression (expected 5 fields): {}", expr);
        return false;
    }

    let minute = now.minute() as u32;
    let hour = now.hour() as u32;
    let dom = now.day();
    let month = now.month();
    let dow = now.weekday().num_days_from_sunday(); // 0=Sun

    field_matches(fields[0], minute, 0, 59)
        && field_matches(fields[1], hour, 0, 23)
        && field_matches(fields[2], dom, 1, 31)
        && field_matches(fields[3], month, 1, 12)
        && field_matches(fields[4], dow, 0, 6)
}

/// Check if a cron field matches a value. Supports: *, N, N-M, */N, N,M,...
fn field_matches(field: &str, value: u32, _min: u32, _max: u32) -> bool {
    if field == "*" {
        return true;
    }

    // Handle comma-separated values
    for part in field.split(',') {
        let part = part.trim();

        // */N — step
        if let Some(step_str) = part.strip_prefix("*/") {
            if let Ok(step) = step_str.parse::<u32>() {
                if step > 0 && value % step == 0 {
                    return true;
                }
            }
            continue;
        }

        // N-M — range
        if part.contains('-') {
            let parts: Vec<&str> = part.split('-').collect();
            if parts.len() == 2 {
                if let (Ok(start), Ok(end)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    if value >= start && value <= end {
                        return true;
                    }
                }
            }
            continue;
        }

        // N — exact
        if let Ok(n) = part.parse::<u32>() {
            if value == n {
                return true;
            }
        }
    }

    false
}

/// Apply a rough timezone offset. Supports common IANA timezone names.
fn apply_timezone_offset(utc: &chrono::DateTime<Utc>, tz_str: &str) -> NaiveDateTime {
    let offset_hours: i64 = match tz_str {
        "America/New_York" | "US/Eastern" | "EST" => {
            // EDT (Mar-Nov) or EST
            if is_dst_eastern(utc) { -4 } else { -5 }
        }
        "America/Chicago" | "US/Central" | "CST" => {
            if is_dst_eastern(utc) { -5 } else { -6 }
        }
        "America/Denver" | "US/Mountain" | "MST" => {
            if is_dst_eastern(utc) { -6 } else { -7 }
        }
        "America/Los_Angeles" | "US/Pacific" | "PST" => {
            if is_dst_eastern(utc) { -7 } else { -8 }
        }
        "UTC" | "GMT" => 0,
        "Europe/London" => if is_dst_eastern(utc) { 1 } else { 0 },
        "Europe/Berlin" | "Europe/Paris" => if is_dst_eastern(utc) { 2 } else { 1 },
        "Asia/Tokyo" | "JST" => 9,
        "Asia/Shanghai" | "Asia/Hong_Kong" => 8,
        _ => {
            debug!("Unknown timezone '{}', using UTC", tz_str);
            0
        }
    };

    let offset = chrono::Duration::hours(offset_hours);
    (utc.naive_utc() + offset)
}

/// Rough US DST check (second Sunday of March to first Sunday of November)
fn is_dst_eastern(utc: &chrono::DateTime<Utc>) -> bool {
    let month = utc.month();
    match month {
        4..=10 => true,  // Apr-Oct always DST
        1..=2 | 12 => false, // Jan-Feb, Dec always standard
        3 => utc.day() > 14, // rough: after March 14
        11 => utc.day() < 7, // rough: before Nov 7
        _ => false,
    }
}

async fn run_cron_agent_turn(config: &GatewayConfig, message: &str) -> Result<AgentTurnResult> {
    let workspace_dir = workspace::resolve_workspace_dir(&config.agent.name);
    let ws_str = workspace_dir.to_string_lossy().to_string();

    let session_key = format!("cron:{}:{}", config.agent.name, uuid::Uuid::new_v4());

    let turn_config = AgentTurnConfig {
        agent_name: config.agent.name.clone(),
        session_key,
        workspace_dir: ws_str,
        minimal_context: true,
    ..AgentTurnConfig::default()
    };

    let provider: Box<dyn LlmProvider> = match FallbackProvider::from_config() {
        Ok(fb) => Box::new(fb),
        Err(e) => return Err(e),
    };

    let mut tools = ToolRegistry::with_defaults();
    tools.load_plugins(&workspace_dir);

    let result = openclaw_agent::runtime::run_agent_turn(
        provider.as_ref(),
        message,
        &turn_config,
        &tools,
    )
    .await?;

    Ok(result)
}

/// Resolve the chat ID for cron delivery.
/// Uses the first allowed user's chat ID (for Telegram, chat_id == user_id for DMs).
fn resolve_delivery_chat(config: &GatewayConfig) -> Option<i64> {
    config.telegram.allowed_user_ids.first().copied()
}

fn cron_jobs_path() -> PathBuf {
    openclaw_core::paths::cron_jobs_path()
}

fn update_job_state(job_id: &str, duration_ms: u128, status: &str) -> Result<()> {
    let path = cron_jobs_path();
    let content = std::fs::read_to_string(&path)?;
    let mut cron_file: serde_json::Value = serde_json::from_str(&content)?;

    if let Some(jobs) = cron_file.get_mut("jobs").and_then(|j| j.as_array_mut()) {
        for job in jobs.iter_mut() {
            if job.get("id").and_then(|v| v.as_str()) == Some(job_id) {
                let now_ms = Utc::now().timestamp_millis();
                let state = serde_json::json!({
                    "lastRunAtMs": now_ms,
                    "lastStatus": status,
                    "lastDurationMs": duration_ms as u64,
                });
                if let Some(existing) = job.get_mut("state") {
                    if let Some(obj) = existing.as_object_mut() {
                        obj.insert("lastRunAtMs".to_string(), serde_json::json!(now_ms));
                        obj.insert("lastStatus".to_string(), serde_json::json!(status));
                        obj.insert("lastDurationMs".to_string(), serde_json::json!(duration_ms as u64));
                        if status == "error" {
                            let prev = obj.get("consecutiveErrors")
                                .and_then(|v| v.as_u64()).unwrap_or(0);
                            obj.insert("consecutiveErrors".to_string(), serde_json::json!(prev + 1));
                        } else {
                            obj.insert("consecutiveErrors".to_string(), serde_json::json!(0));
                        }
                    }
                } else {
                    job.as_object_mut().unwrap().insert("state".to_string(), state);
                }
                break;
            }
        }
    }

    let updated = serde_json::to_string_pretty(&cron_file)?;
    std::fs::write(&path, updated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_matches_star() {
        assert!(field_matches("*", 5, 0, 59));
        assert!(field_matches("*", 0, 0, 59));
    }

    #[test]
    fn test_field_matches_exact() {
        assert!(field_matches("30", 30, 0, 59));
        assert!(!field_matches("30", 31, 0, 59));
    }

    #[test]
    fn test_field_matches_range() {
        assert!(field_matches("1-5", 3, 1, 7));
        assert!(!field_matches("1-5", 6, 1, 7));
    }

    #[test]
    fn test_field_matches_step() {
        assert!(field_matches("*/15", 0, 0, 59));
        assert!(field_matches("*/15", 15, 0, 59));
        assert!(field_matches("*/15", 30, 0, 59));
        assert!(!field_matches("*/15", 10, 0, 59));
    }

    #[test]
    fn test_field_matches_comma() {
        assert!(field_matches("1,3,5", 3, 0, 6));
        assert!(!field_matches("1,3,5", 4, 0, 6));
    }

    #[test]
    fn test_matches_cron_expr() {
        // "30 6 * * *" = 6:30 AM every day
        let dt = NaiveDateTime::parse_from_str("2026-02-18 06:30:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert!(matches_cron_expr("30 6 * * *", &dt));

        let dt2 = NaiveDateTime::parse_from_str("2026-02-18 06:31:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert!(!matches_cron_expr("30 6 * * *", &dt2));
    }

    #[test]
    fn test_matches_cron_midnight() {
        let dt = NaiveDateTime::parse_from_str("2026-02-18 00:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        assert!(matches_cron_expr("0 0 * * *", &dt));
    }
}
