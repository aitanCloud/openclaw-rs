use serde::{Deserialize, Serialize};

/// Gateway configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
    pub agent: AgentConfig,
    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub allowed_user_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub fallback: bool,
    pub model: Option<String>,
    #[serde(default)]
    pub sandbox: Option<SandboxConfig>,
}

/// Optional sandbox configuration overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Extra commands to block (added to defaults)
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    /// Max exec timeout in seconds
    pub max_exec_timeout_secs: Option<u64>,
    /// Per-turn timeout in seconds
    pub turn_timeout_secs: Option<u64>,
    /// Subagent (delegate) timeout in seconds (default 300)
    pub subagent_timeout_secs: Option<u64>,
    /// Max concurrent tasks
    pub max_concurrent: Option<usize>,
    /// Rate limit: messages per window
    pub rate_limit_messages: Option<usize>,
    /// Rate limit: window in seconds
    pub rate_limit_window_secs: Option<u64>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let json = r#"{
            "telegram": { "bot_token": "123:ABC", "allowed_user_ids": [42] },
            "agent": { "name": "test" }
        }"#;
        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.telegram.bot_token, "123:ABC");
        assert_eq!(config.telegram.allowed_user_ids, vec![42]);
        assert_eq!(config.agent.name, "test");
        assert!(config.agent.fallback); // default_true
        assert!(config.agent.model.is_none());
        assert!(config.discord.is_none());
    }

    #[test]
    fn test_parse_full_config_with_discord() {
        let json = r#"{
            "telegram": { "bot_token": "tg-token", "allowed_user_ids": [] },
            "discord": { "bot_token": "dc-token", "allowed_user_ids": [1, 2] },
            "agent": { "name": "main", "fallback": false, "model": "gpt-4" }
        }"#;
        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        assert!(!config.agent.fallback);
        assert_eq!(config.agent.model.as_deref(), Some("gpt-4"));
        let dc = config.discord.unwrap();
        assert_eq!(dc.bot_token, "dc-token");
        assert_eq!(dc.allowed_user_ids, vec![1, 2]);
    }

    #[test]
    fn test_parse_sandbox_config() {
        let json = r#"{
            "telegram": { "bot_token": "t", "allowed_user_ids": [] },
            "agent": {
                "name": "a",
                "sandbox": {
                    "blocked_commands": ["rm", "dd"],
                    "max_exec_timeout_secs": 30,
                    "turn_timeout_secs": 120,
                    "max_concurrent": 2
                }
            }
        }"#;
        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        let sb = config.agent.sandbox.unwrap();
        assert_eq!(sb.blocked_commands, vec!["rm", "dd"]);
        assert_eq!(sb.max_exec_timeout_secs, Some(30));
        assert_eq!(sb.turn_timeout_secs, Some(120));
        assert_eq!(sb.max_concurrent, Some(2));
    }

    #[test]
    fn test_parse_webhook_config() {
        let json = r#"{
            "telegram": { "bot_token": "t", "allowed_user_ids": [] },
            "agent": { "name": "a" },
            "webhook": { "token": "secret-token-123" }
        }"#;
        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        let wh = config.webhook.unwrap();
        assert_eq!(wh.token, "secret-token-123");
    }

    #[test]
    fn test_webhook_config_optional() {
        let json = r#"{
            "telegram": { "bot_token": "t", "allowed_user_ids": [] },
            "agent": { "name": "a" }
        }"#;
        let config: GatewayConfig = serde_json::from_str(json).unwrap();
        assert!(config.webhook.is_none());
    }
}

impl GatewayConfig {
    /// Load from environment variables
    pub fn from_env() -> anyhow::Result<Self> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN not set"))?;

        let allowed_ids: Vec<i64> = std::env::var("ALLOWED_USER_IDS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "main".to_string());
        let use_fallback = std::env::var("USE_FALLBACK")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(true);
        let model = std::env::var("MODEL").ok();

        Ok(Self {
            telegram: TelegramConfig {
                bot_token,
                allowed_user_ids: allowed_ids,
            },
            discord: None,
            agent: AgentConfig {
                name: agent_name,
                fallback: use_fallback,
                model,
                sandbox: None,
            },
            webhook: None,
        })
    }

    /// Load from a JSON config file, with env overrides
    pub fn from_file_or_env(path: &str) -> anyhow::Result<Self> {
        if let Ok(content) = std::fs::read_to_string(path) {
            let mut config: Self = serde_json::from_str(&content)?;
            // Allow env overrides
            if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
                config.telegram.bot_token = token;
            }
            if let Ok(ids) = std::env::var("ALLOWED_USER_IDS") {
                config.telegram.allowed_user_ids = ids
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            Ok(config)
        } else {
            Self::from_env()
        }
    }
}
