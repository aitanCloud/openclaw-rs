use serde::{Deserialize, Serialize};

/// Gateway configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
    pub agent: AgentConfig,
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
