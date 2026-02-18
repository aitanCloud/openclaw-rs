use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct CronFile {
    pub version: u32,
    pub jobs: Vec<CronJob>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub created_at_ms: Option<u64>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
    pub schedule: CronSchedule,
    #[serde(default)]
    pub session_target: Option<String>,
    #[serde(default)]
    pub wake_mode: Option<String>,
    pub payload: CronPayload,
    #[serde(default)]
    pub delivery: Option<CronDelivery>,
    #[serde(default)]
    pub state: Option<CronState>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CronSchedule {
    #[serde(rename = "cron")]
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    #[serde(rename = "every")]
    Every {
        #[serde(rename = "everyMs")]
        every_ms: u64,
        #[serde(default, rename = "anchorMs")]
        anchor_ms: Option<u64>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronPayload {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronDelivery {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronState {
    #[serde(default)]
    pub next_run_at_ms: Option<u64>,
    #[serde(default)]
    pub last_run_at_ms: Option<u64>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_duration_ms: Option<u64>,
    #[serde(default)]
    pub consecutive_errors: Option<u32>,
}

/// Load and parse cron/jobs.json
pub fn load_cron_jobs(path: &Path) -> Result<CronFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read cron jobs: {}", path.display()))?;
    let cron_file: CronFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse cron jobs: {}", path.display()))?;
    Ok(cron_file)
}

impl std::fmt::Display for CronSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronSchedule::Cron { expr, tz } => {
                write!(f, "cron: {}", expr)?;
                if let Some(tz) = tz {
                    write!(f, " ({})", tz)?;
                }
                Ok(())
            }
            CronSchedule::Every { every_ms, .. } => {
                let hours = *every_ms / 3_600_000;
                let mins = (*every_ms % 3_600_000) / 60_000;
                if hours > 0 {
                    write!(f, "every {}h", hours)?;
                    if mins > 0 {
                        write!(f, " {}m", mins)?;
                    }
                } else {
                    write!(f, "every {}m", mins)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cron_jobs() {
        let json = r#"{
            "version": 1,
            "jobs": [{
                "id": "test-id",
                "name": "Test job",
                "enabled": true,
                "schedule": { "kind": "cron", "expr": "30 6 * * *", "tz": "America/New_York" },
                "payload": { "kind": "agentTurn", "message": "hello" }
            }]
        }"#;
        let cron_file: CronFile = serde_json::from_str(json).unwrap();
        assert_eq!(cron_file.jobs.len(), 1);
        assert_eq!(cron_file.jobs[0].name, "Test job");
        assert!(matches!(&cron_file.jobs[0].schedule, CronSchedule::Cron { expr, .. } if expr == "30 6 * * *"));
    }

    #[test]
    fn test_parse_every_schedule() {
        let json = r#"{
            "version": 1,
            "jobs": [{
                "id": "test-id",
                "name": "Frequent job",
                "enabled": true,
                "schedule": { "kind": "every", "everyMs": 7200000 },
                "payload": { "kind": "agentTurn", "message": "check" }
            }]
        }"#;
        let cron_file: CronFile = serde_json::from_str(json).unwrap();
        let sched = &cron_file.jobs[0].schedule;
        assert_eq!(format!("{}", sched), "every 2h");
    }
}
