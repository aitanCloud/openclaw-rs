use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct SessionsFile {
    #[serde(default)]
    pub sessions: Vec<SessionEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub message_count: Option<u32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

/// Load sessions.json for a given agent
pub fn load_sessions(path: &Path) -> Result<SessionsFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read sessions: {}", path.display()))?;
    let sessions: SessionsFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse sessions: {}", path.display()))?;
    Ok(sessions)
}

/// List all agent directories that have sessions
pub fn list_agents_with_sessions() -> Result<Vec<String>> {
    let agents_dir = crate::paths::agents_dir();
    if !agents_dir.exists() {
        return Ok(vec![]);
    }

    let mut agents = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)
        .with_context(|| format!("Failed to read agents dir: {}", agents_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let sessions_file = entry.path().join("sessions").join("sessions.json");
            if sessions_file.exists() {
                if let Some(name) = entry.file_name().to_str() {
                    agents.push(name.to_string());
                }
            }
        }
    }
    agents.sort();
    Ok(agents)
}
