use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Top-level openclaw-manual.json structure
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualConfig {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub env: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub browser: serde_json::Value,
    #[serde(default)]
    pub models: serde_json::Value,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub tools: serde_json::Value,
    #[serde(default)]
    pub commands: serde_json::Value,
    #[serde(default)]
    pub channels: serde_json::Value,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub plugins: serde_json::Value,
    #[serde(default)]
    pub session: serde_json::Value,
    #[serde(default)]
    pub messages: serde_json::Value,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    #[serde(default)]
    pub defaults: AgentDefaults,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaults {
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub context_pruning: Option<serde_json::Value>,
    #[serde(default)]
    pub compaction: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub auth: Option<String>,
}

/// Load and parse openclaw-manual.json
pub fn load_manual_config(path: &Path) -> Result<ManualConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config: {}", path.display()))?;
    let config: ManualConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse config: {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let json = r#"{"meta": {"name": "test"}, "gateway": {"port": 3001}}"#;
        let config: ManualConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.meta.name.as_deref(), Some("test"));
        assert_eq!(config.gateway.port, Some(3001));
    }

    #[test]
    fn test_parse_empty_config() {
        let json = "{}";
        let config: ManualConfig = serde_json::from_str(json).unwrap();
        assert!(config.meta.name.is_none());
        assert!(config.gateway.port.is_none());
    }
}
