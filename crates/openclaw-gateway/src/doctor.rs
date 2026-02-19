//! Doctor module — runs health checks and returns a diagnostic report.
//! Each check returns (name, passed, detail).

use std::path::Path;
use tracing::debug;

fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();
            if let Ok(m) = meta {
                if m.is_file() {
                    total += m.len();
                } else if m.is_dir() {
                    total += dir_size_bytes(&entry.path());
                }
            }
        }
    }
    total
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Public wrapper for human_bytes (used by main.rs /health endpoint)
pub fn human_bytes_pub(bytes: u64) -> String {
    human_bytes(bytes)
}

/// Run all health checks and return a list of (check_name, passed, detail)
pub async fn run_checks(agent_name: &str) -> Vec<(String, bool, String)> {
    let mut checks = Vec::new();

    // 1. Workspace directory exists
    let workspace_dir = openclaw_agent::workspace::resolve_workspace_dir(agent_name);
    let ws_exists = workspace_dir.exists();
    checks.push((
        "Workspace".to_string(),
        ws_exists,
        if ws_exists {
            format!("{}", workspace_dir.display())
        } else {
            format!("Missing: {}", workspace_dir.display())
        },
    ));

    // 2. Config file exists
    let config_path = openclaw_core::paths::manual_config_path();
    let config_exists = config_path.exists();
    checks.push((
        "Config".to_string(),
        config_exists,
        if config_exists {
            "openclaw-manual.json found".to_string()
        } else {
            format!("Missing: {}", config_path.display())
        },
    ));

    // 3. Session database accessible
    let session_ok = openclaw_agent::sessions::SessionStore::open(agent_name).is_ok();
    checks.push((
        "Sessions DB".to_string(),
        session_ok,
        if session_ok {
            "SQLite accessible".to_string()
        } else {
            "Failed to open session database".to_string()
        },
    ));

    // 4. Skills directory
    let skills_dir = workspace_dir.join("skills");
    let skills = openclaw_core::skills::list_skills(&skills_dir).unwrap_or_default();
    checks.push((
        "Skills".to_string(),
        true, // skills are optional, always "ok"
        format!("{} skill(s) found", skills.len()),
    ));

    // 5. LLM provider reachable (try to build from config)
    match openclaw_agent::llm::fallback::FallbackProvider::from_config() {
        Ok(fb) => {
            let labels = fb.provider_labels();
            checks.push((
                "LLM Provider".to_string(),
                true,
                format!("{} model(s): {}", labels.len(), labels.join(", ")),
            ));
        }
        Err(_) => {
            checks.push((
                "LLM Provider".to_string(),
                false,
                "No providers configured".to_string(),
            ));
        }
    }

    // 6. Metrics available
    let metrics_ok = crate::metrics::global().is_some();
    checks.push((
        "Metrics".to_string(),
        metrics_ok,
        if metrics_ok {
            "Initialized".to_string()
        } else {
            "Not initialized".to_string()
        },
    ));

    // 7. Cron jobs
    let cron_path = openclaw_core::paths::cron_jobs_path();
    let cron_detail = if cron_path.exists() {
        match std::fs::read_to_string(&cron_path) {
            Ok(content) => {
                let v: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();
                let jobs = v.get("jobs").and_then(|j| j.as_array()).map(|a| a.len()).unwrap_or(0);
                let enabled = v.get("jobs").and_then(|j| j.as_array())
                    .map(|a| a.iter().filter(|j| j.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false)).count())
                    .unwrap_or(0);
                format!("{} job(s), {} enabled", jobs, enabled)
            }
            Err(_) => "Failed to read cron/jobs.json".to_string(),
        }
    } else {
        "No cron/jobs.json found".to_string()
    };
    checks.push(("Cron Jobs".to_string(), true, cron_detail));

    // 8. Disk usage (workspace + sessions DB)
    let ws_size = dir_size_bytes(&workspace_dir);
    let db_path = openclaw_core::paths::agent_sessions_dir(agent_name).join("sessions.db");
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    checks.push((
        "Disk Usage".to_string(),
        true,
        format!("workspace: {}, sessions DB: {}", human_bytes(ws_size), human_bytes(db_size)),
    ));

    // 9. Webhook
    checks.push((
        "Webhook".to_string(),
        true,
        if std::env::var("WEBHOOK_TOKEN").is_ok() || cron_path.exists() {
            "Configured".to_string()
        } else {
            "Not configured".to_string()
        },
    ));

    // 10. Memory (RSS)
    let rss = crate::process_rss_bytes();
    let rss_mb = rss as f64 / 1_048_576.0;
    let mem_ok = rss_mb < 512.0;
    checks.push((
        "Memory".to_string(),
        mem_ok,
        if mem_ok {
            format!("{} RSS", human_bytes(rss))
        } else {
            format!("⚠ {:.0} MB RSS (>512 MB threshold)", rss_mb)
        },
    ));

    // 11. Active tasks
    let active = crate::task_registry::active_count();
    checks.push((
        "Active Tasks".to_string(),
        true,
        format!("{} running", active),
    ));

    // 12. Uptime
    let uptime_secs = crate::handler::BOOT_TIME.elapsed().as_secs();
    checks.push((
        "Uptime".to_string(),
        true,
        crate::human_uptime(uptime_secs),
    ));

    debug!("Doctor: {}/{} checks passed",
        checks.iter().filter(|(_, ok, _)| *ok).count(),
        checks.len(),
    );

    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_doctor_returns_checks() {
        let checks = run_checks("test-agent").await;
        // Should always return at least 12 checks
        assert!(checks.len() >= 12, "Expected >=12 checks, got {}", checks.len());

        // Verify check names are present
        let names: Vec<&str> = checks.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"Workspace"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Sessions DB"));
        assert!(names.contains(&"Skills"));
        assert!(names.contains(&"LLM Provider"));
        assert!(names.contains(&"Metrics"));
        assert!(names.contains(&"Cron Jobs"));
        assert!(names.contains(&"Disk Usage"));
        assert!(names.contains(&"Webhook"));
        assert!(names.contains(&"Memory"));
        assert!(names.contains(&"Active Tasks"));
        assert!(names.contains(&"Uptime"));
    }

    #[tokio::test]
    async fn test_doctor_skills_always_ok() {
        let checks = run_checks("nonexistent-agent").await;
        let skills_check = checks.iter().find(|(n, _, _)| n == "Skills").unwrap();
        assert!(skills_check.1, "Skills check should always pass");
    }

    #[test]
    fn test_human_bytes_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
    }

    #[test]
    fn test_human_bytes_kb() {
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
    }

    #[test]
    fn test_human_bytes_mb() {
        assert_eq!(human_bytes(1_048_576), "1.0 MB");
        assert_eq!(human_bytes(5_242_880), "5.0 MB");
    }

    #[test]
    fn test_human_bytes_gb() {
        assert_eq!(human_bytes(1_073_741_824), "1.0 GB");
        assert_eq!(human_bytes(2_684_354_560), "2.5 GB");
    }

    #[test]
    fn test_dir_size_bytes_nonexistent() {
        assert_eq!(dir_size_bytes(Path::new("/nonexistent/path/xyz")), 0);
    }
}
