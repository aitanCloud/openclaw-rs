//! Doctor module — runs health checks and returns a diagnostic report.
//! Each check returns (name, passed, detail).

use tracing::debug;

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

    // 7. Active tasks
    let active = crate::task_registry::active_count();
    checks.push((
        "Active Tasks".to_string(),
        true,
        format!("{} running", active),
    ));

    // 8. Uptime
    let uptime = crate::handler::BOOT_TIME.elapsed();
    let hours = uptime.as_secs() / 3600;
    let mins = (uptime.as_secs() % 3600) / 60;
    checks.push((
        "Uptime".to_string(),
        true,
        format!("{}h {}m", hours, mins),
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
        // Should always return at least 8 checks
        assert!(checks.len() >= 8, "Expected >=8 checks, got {}", checks.len());

        // Verify check names are present
        let names: Vec<&str> = checks.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"Workspace"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Sessions DB"));
        assert!(names.contains(&"Skills"));
        assert!(names.contains(&"LLM Provider"));
        assert!(names.contains(&"Metrics"));
        assert!(names.contains(&"Active Tasks"));
        assert!(names.contains(&"Uptime"));
    }

    #[tokio::test]
    async fn test_doctor_skills_always_ok() {
        let checks = run_checks("nonexistent-agent").await;
        let skills_check = checks.iter().find(|(n, _, _)| n == "Skills").unwrap();
        assert!(skills_check.1, "Skills check should always pass");
    }
}
