use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;

/// Resolve the gateway URL from env or default
fn gateway_url() -> String {
    std::env::var("OPENCLAW_GATEWAY_URL").unwrap_or_else(|_| "http://localhost:3100".to_string())
}

#[derive(Subcommand)]
pub enum GatewayAction {
    /// Show gateway health status
    Status,
    /// Show recent LLM activity log
    Logs {
        /// Number of entries to show (default 10)
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Run doctor health checks
    Doctor,
    /// Show gateway version info
    Version,
    /// Ping the gateway
    Ping,
    /// Show Prometheus metrics summary
    Metrics,
    /// Show current model and fallback chain
    Model,
    /// List recent sessions from the gateway
    Sessions {
        /// Maximum number of sessions to show
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
}

pub async fn run(action: GatewayAction) -> Result<()> {
    let base = gateway_url();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    match action {
        GatewayAction::Status => status(&client, &base).await,
        GatewayAction::Logs { limit } => logs(&client, &base, limit).await,
        GatewayAction::Doctor => doctor(&client, &base).await,
        GatewayAction::Version => version(&client, &base).await,
        GatewayAction::Ping => ping(&client, &base).await,
        GatewayAction::Metrics => metrics(&client, &base).await,
        GatewayAction::Model => model(&client, &base).await,
        GatewayAction::Sessions { limit } => sessions(&client, &base, limit).await,
    }
}

async fn status(client: &reqwest::Client, base: &str) -> Result<()> {
    let resp = client.get(format!("{}/health/lite", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    println!("{}", "═══ Gateway Status ═══".bold().cyan());
    println!("  {} {}", "Status:".bold(), format_status(json["status"].as_str().unwrap_or("unknown")));
    println!("  {} v{}", "Version:".bold(), json["version"].as_str().unwrap_or("?"));
    println!("  {} {}", "Agent:".bold(), json["agent"].as_str().unwrap_or("?"));
    println!("  {} {}", "Uptime:".bold(), json["uptime"].as_str().unwrap_or("?"));
    println!("  {} {}", "PID:".bold(), json["pid"]);
    println!("  {} {}", "Active tasks:".bold(), json["active_tasks"]);
    println!("  {} {}", "Providers:".bold(), json["provider_count"]);
    Ok(())
}

async fn logs(client: &reqwest::Client, base: &str, limit: usize) -> Result<()> {
    let resp = client.get(format!("{}/logs?limit={}", base, limit)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    let total = json["total_recorded"].as_u64().unwrap_or(0);
    let entries = json["entries"].as_array();

    println!("{}", "═══ LLM Activity Log ═══".bold().cyan());
    println!("  {} total recorded\n", total.to_string().bold());

    if let Some(entries) = entries {
        if entries.is_empty() {
            println!("  {}", "No activity yet.".dimmed());
            return Ok(());
        }
        for (i, entry) in entries.iter().enumerate() {
            let model = entry["model"].as_str().unwrap_or("?");
            let latency = entry["latency_ms"].as_u64().unwrap_or(0);
            let tokens = entry["usage_total_tokens"].as_u64().unwrap_or(0);
            let has_error = entry["error"].is_string();
            let streaming = entry["streaming"].as_bool().unwrap_or(false);
            let timestamp = entry["timestamp"].as_str().unwrap_or("");

            let status_icon = if has_error { "❌".red().to_string() } else { "✅".green().to_string() };
            let stream_tag = if streaming { " [stream]".dimmed().to_string() } else { String::new() };

            println!("  {}. {} {} {}ms {}tok{}",
                (i + 1).to_string().bold(),
                status_icon,
                model.yellow(),
                latency,
                tokens,
                stream_tag,
            );
            if !timestamp.is_empty() {
                println!("     {}", timestamp.dimmed());
            }

            // Show content preview or error
            if has_error {
                let err = entry["error"].as_str().unwrap_or("unknown error");
                let preview = if err.len() > 100 { &err[..100] } else { err };
                println!("     {}", format!("ERROR: {}", preview).red());
            } else if let Some(content) = entry["response_content"].as_str() {
                let preview = if content.len() > 100 {
                    format!("{}…", &content[..97])
                } else {
                    content.to_string()
                };
                println!("     {}", preview.dimmed());
            } else {
                let tool_calls = entry["response_tool_calls"].as_u64().unwrap_or(0);
                if tool_calls > 0 {
                    let names = entry["tool_call_names"].as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default();
                    println!("     {}", format!("[{} tool(s): {}]", tool_calls, names).dimmed());
                }
            }
            println!();
        }
    }
    Ok(())
}

async fn doctor(client: &reqwest::Client, base: &str) -> Result<()> {
    let resp = client.get(format!("{}/doctor/json", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    let all_ok = json["all_ok"].as_bool().unwrap_or(false);
    let total = json["checks_total"].as_u64().unwrap_or(0);
    let passed = json["checks_passed"].as_u64().unwrap_or(0);

    let header = if all_ok {
        "═══ Doctor: All Healthy ═══".bold().green()
    } else {
        "═══ Doctor: Issues Found ═══".bold().yellow()
    };
    println!("{}", header);
    println!("  {}/{} checks passed\n", passed, total);

    if let Some(checks) = json["checks"].as_array() {
        for check in checks {
            let name = check["name"].as_str().unwrap_or("?");
            let ok = check["ok"].as_bool().unwrap_or(false);
            let detail = check["detail"].as_str().unwrap_or("");
            let icon = if ok { "✅" } else { "❌" };
            let detail_colored = if ok { detail.normal() } else { detail.red() };
            println!("  {} {} — {}", icon, name.bold(), detail_colored);
        }
    }
    Ok(())
}

async fn version(client: &reqwest::Client, base: &str) -> Result<()> {
    let resp = client.get(format!("{}/version", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    println!("{}", "═══ Gateway Version ═══".bold().cyan());
    println!("  {} v{}", "Version:".bold(), json["version"].as_str().unwrap_or("?"));
    println!("  {} {}", "Agent:".bold(), json["agent"].as_str().unwrap_or("?"));
    println!("  {} {}", "Built:".bold(), json["built"].as_str().unwrap_or("?"));
    println!("  {} {}", "Boot:".bold(), json["boot_time"].as_str().unwrap_or("?"));
    println!("  {} {}", "Rust:".bold(), json["rust_version"].as_str().unwrap_or("?"));
    Ok(())
}

async fn ping(client: &reqwest::Client, base: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    let resp = client.get(format!("{}/ping", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let body = resp.text().await?;
    let ms = t0.elapsed().as_millis();

    let color = if ms < 50 { "green" } else if ms < 200 { "yellow" } else { "red" };
    let ms_str = format!("{}ms", ms);
    let ms_colored = match color {
        "green" => ms_str.green(),
        "yellow" => ms_str.yellow(),
        _ => ms_str.red(),
    };
    println!("{} {} ({})", "🏓".bold(), body, ms_colored);
    Ok(())
}

async fn metrics(client: &reqwest::Client, base: &str) -> Result<()> {
    let resp = client.get(format!("{}/metrics/summary", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let body = resp.text().await?;

    println!("{}", "═══ Metrics Summary ═══".bold().cyan());
    // Parse the key=value format
    for pair in body.split_whitespace() {
        if let Some((key, val)) = pair.split_once('=') {
            println!("  {} {}", format!("{}:", key).bold(), val);
        }
    }
    Ok(())
}

async fn model(client: &reqwest::Client, base: &str) -> Result<()> {
    let resp = client.get(format!("{}/status", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    println!("{}", "═══ Model Info ═══".bold().cyan());
    println!("  {} {}", "Agent:".bold(), json["agent"].as_str().unwrap_or("?"));
    println!("  {} {}", "Fallback:".bold(),
        if json["fallback"].as_bool().unwrap_or(false) { "enabled".green() } else { "disabled".red() });

    if let Some(model) = json["model"].as_str() {
        println!("  {} {}", "Model:".bold(), model.yellow());
    }

    if let Some(providers) = json["providers"].as_array() {
        if !providers.is_empty() {
            println!("\n  {}", "Fallback Chain:".bold());
            for (i, p) in providers.iter().enumerate() {
                let label = p.as_str().unwrap_or("?");
                let marker = match i {
                    0 => "🥇",
                    1 => "🥈",
                    2 => "🥉",
                    _ => "  ",
                };
                println!("  {} {}", marker, label.yellow());
            }
            println!("\n  {}", "Circuit breaker: >3 consecutive failures = provider skipped".dimmed());
        }
    }
    Ok(())
}

async fn sessions(client: &reqwest::Client, base: &str, limit: usize) -> Result<()> {
    let resp = client.get(format!("{}/status", base)).send().await
        .map_err(|e| anyhow::anyhow!("Cannot reach gateway at {}: {}", base, e))?;
    let json: serde_json::Value = resp.json().await?;

    println!("{}", "═══ Sessions ═══".bold().cyan());

    if let Some(sessions) = json.get("sessions") {
        let total = sessions["total"].as_u64().unwrap_or(0);
        let tg = sessions["telegram"].as_u64().unwrap_or(0);
        let dc = sessions["discord"].as_u64().unwrap_or(0);
        let msgs = sessions["total_messages"].as_u64().unwrap_or(0);
        let tokens = sessions["total_tokens"].as_u64().unwrap_or(0);

        println!("  {} {}", "Total:".bold(), total);
        println!("  {} {}", "Telegram:".bold(), tg);
        println!("  {} {}", "Discord:".bold(), dc);
        println!("  {} {}", "Messages:".bold(), msgs);
        println!("  {} {}", "Tokens:".bold(), tokens);
    } else {
        println!("  {}", "Session data not available from gateway.".dimmed());
    }

    // Also show recent sessions from the detailed list if available
    if let Some(session_list) = json.get("session_list").and_then(|v| v.as_array()) {
        println!("\n  {}", "Recent Sessions:".bold());
        for (i, s) in session_list.iter().take(limit).enumerate() {
            let key = s["key"].as_str().unwrap_or("?");
            let msgs = s["messages"].as_u64().unwrap_or(0);
            let tokens = s["tokens"].as_u64().unwrap_or(0);
            let short_key = if key.len() > 40 { &key[..40] } else { key };
            println!("  {}. {} — {} msgs, {} tok",
                (i + 1).to_string().bold(), short_key.dimmed(), msgs, tokens);
        }
    }
    Ok(())
}

fn format_status(status: &str) -> colored::ColoredString {
    match status {
        "ok" => "ok".green().bold(),
        "degraded" => "degraded".yellow().bold(),
        _ => status.to_string().red().bold(),
    }
}
