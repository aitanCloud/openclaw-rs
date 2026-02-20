use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};

pub struct CronTool;

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &str {
        "cron"
    }

    fn description(&self) -> &str {
        "Manage cron jobs. Actions: list (show all jobs), enable/disable (toggle by name), add (create new job), remove (delete by name)."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "enable", "disable", "add", "remove"],
                    "description": "Action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Job name (for enable/disable/remove, case-insensitive partial match)"
                },
                "schedule": {
                    "type": "string",
                    "description": "Cron expression e.g. '30 6 * * *' (for 'add' action)"
                },
                "message": {
                    "type": "string",
                    "description": "The message/prompt to send when the job fires (for 'add' action)"
                },
                "timezone": {
                    "type": "string",
                    "description": "Timezone e.g. 'America/New_York' (for 'add' action, optional)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cron: missing 'action' argument"))?;

        let cron_path = openclaw_core::paths::cron_jobs_path();

        match action {
            "list" => list_jobs(&cron_path),
            "enable" => toggle_job(&cron_path, &args, true),
            "disable" => toggle_job(&cron_path, &args, false),
            "add" => add_job(&cron_path, &args),
            "remove" => remove_job(&cron_path, &args),
            _ => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Use: list, enable, disable, add, remove",
                action
            ))),
        }
    }
}

fn list_jobs(path: &std::path::Path) -> Result<ToolResult> {
    if !path.exists() {
        return Ok(ToolResult::success("No cron jobs file found."));
    }

    let cron_file = openclaw_core::cron::load_cron_jobs(path)?;
    if cron_file.jobs.is_empty() {
        return Ok(ToolResult::success("No cron jobs configured."));
    }

    let mut lines = Vec::new();
    for job in &cron_file.jobs {
        let status = if job.enabled { "✅" } else { "⏸️" };
        let last_run = job
            .state
            .as_ref()
            .and_then(|s| s.last_run_at_ms)
            .map(|ms| {
                let dt = chrono::DateTime::from_timestamp_millis(ms as i64)
                    .unwrap_or_default();
                dt.format("%Y-%m-%d %H:%M UTC").to_string()
            })
            .unwrap_or_else(|| "never".to_string());

        lines.push(format!(
            "{} {} — {} — last: {} — \"{}\"",
            status, job.name, job.schedule, last_run, job.payload.message
        ));
    }

    Ok(ToolResult::success(lines.join("\n")))
}

fn toggle_job(path: &std::path::Path, args: &Value, enable: bool) -> Result<ToolResult> {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Ok(ToolResult::error("cron enable/disable: missing 'name'")),
    };

    if !path.exists() {
        return Ok(ToolResult::error("No cron jobs file found."));
    }

    let content = std::fs::read_to_string(path)?;
    let mut cron_file: Value = serde_json::from_str(&content)?;

    let jobs = cron_file
        .get_mut("jobs")
        .and_then(|j| j.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("Invalid cron jobs file"))?;

    let name_lower = name.to_lowercase();
    let mut found = None;

    for job in jobs.iter_mut() {
        let job_name = job.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if job_name.to_lowercase().contains(&name_lower) {
            job.as_object_mut()
                .unwrap()
                .insert("enabled".to_string(), serde_json::json!(enable));
            found = Some(job_name);
            break;
        }
    }

    match found {
        Some(job_name) => {
            let updated = serde_json::to_string_pretty(&cron_file)?;
            std::fs::write(path, updated)?;
            let verb = if enable { "Enabled" } else { "Disabled" };
            Ok(ToolResult::success(format!("{} cron job: {}", verb, job_name)))
        }
        None => Ok(ToolResult::error(format!(
            "No cron job matching '{}' found",
            name
        ))),
    }
}

fn add_job(path: &std::path::Path, args: &Value) -> Result<ToolResult> {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Ok(ToolResult::error("cron add: missing 'name'")),
    };
    let schedule = match args.get("schedule").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Ok(ToolResult::error("cron add: missing 'schedule' (cron expression)")),
    };
    let message = match args.get("message").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => return Ok(ToolResult::error("cron add: missing 'message'")),
    };
    let timezone = args.get("timezone").and_then(|v| v.as_str());

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    let id = uuid::Uuid::new_v4().to_string();

    let mut schedule_obj = serde_json::json!({
        "kind": "cron",
        "expr": schedule,
    });
    if let Some(tz) = timezone {
        schedule_obj["tz"] = serde_json::json!(tz);
    }

    let new_job = serde_json::json!({
        "id": id,
        "name": name,
        "enabled": true,
        "createdAtMs": now_ms,
        "updatedAtMs": now_ms,
        "schedule": schedule_obj,
        "payload": {
            "kind": "agentTurn",
            "message": message,
        }
    });

    let mut cron_file: Value = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({"version": 1, "jobs": []})
    };

    cron_file
        .get_mut("jobs")
        .and_then(|j| j.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("Invalid cron jobs file"))?
        .push(new_job);

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let updated = serde_json::to_string_pretty(&cron_file)?;
    std::fs::write(path, updated)?;

    Ok(ToolResult::success(format!(
        "Added cron job '{}' with schedule '{}' — id={}",
        name, schedule, id
    )))
}

fn remove_job(path: &std::path::Path, args: &Value) -> Result<ToolResult> {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Ok(ToolResult::error("cron remove: missing 'name'")),
    };

    if !path.exists() {
        return Ok(ToolResult::error("No cron jobs file found."));
    }

    let content = std::fs::read_to_string(path)?;
    let mut cron_file: Value = serde_json::from_str(&content)?;

    let jobs = cron_file
        .get_mut("jobs")
        .and_then(|j| j.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("Invalid cron jobs file"))?;

    let name_lower = name.to_lowercase();
    let before_len = jobs.len();

    let mut removed_name = None;
    jobs.retain(|job| {
        let job_name = job.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if job_name.to_lowercase().contains(&name_lower) && removed_name.is_none() {
            removed_name = Some(job_name.to_string());
            false
        } else {
            true
        }
    });

    if jobs.len() == before_len {
        return Ok(ToolResult::error(format!(
            "No cron job matching '{}' found",
            name
        )));
    }

    let updated = serde_json::to_string_pretty(&cron_file)?;
    std::fs::write(path, updated)?;

    Ok(ToolResult::success(format!(
        "Removed cron job: {}",
        removed_name.unwrap_or_else(|| name.to_string())
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        ..ToolContext::default()
        }
    }

    #[tokio::test]
    async fn test_cron_list_no_file() {
        let tool = CronTool;
        let ctx = test_ctx();
        // Use a path that doesn't exist
        let args = serde_json::json!({"action": "list"});
        let result = tool.execute(args, &ctx).await.unwrap();
        // Should not error — just say no file
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_cron_missing_action() {
        let tool = CronTool;
        let ctx = test_ctx();
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_cron_unknown_action() {
        let tool = CronTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "explode"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_cron_enable_missing_name() {
        let tool = CronTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "enable"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[tokio::test]
    async fn test_cron_add_missing_fields() {
        let tool = CronTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "add", "name": "test"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }
}
