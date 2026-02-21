use anyhow::Result;
use colored::Colorize;
use openclaw_agent::llm::fallback::FallbackProvider;
use openclaw_agent::llm::{LlmProvider, OpenAiCompatibleProvider};
use openclaw_agent::runtime::{self, AgentTurnConfig};
use openclaw_agent::tools::ToolRegistry;
use openclaw_agent::workspace;
use std::time::Instant;

pub struct AgentOptions {
    pub message: String,
    pub agent: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub stream: bool,
    pub session: Option<String>,
    pub continue_session: bool,
    pub fallback: bool,
}

pub async fn run(opts: AgentOptions) -> Result<()> {
    let t_start = Instant::now();

    let workspace_dir = workspace::resolve_workspace_dir(&opts.agent);
    if !workspace_dir.exists() {
        anyhow::bail!(
            "Workspace not found: {}. Run 'openclaw' (Node.js) first to initialize.",
            workspace_dir.display()
        );
    }

    // ── Resolve provider ──
    let provider: Box<dyn LlmProvider> = if opts.fallback {
        let fb = FallbackProvider::from_config()?;
        let labels = fb.provider_labels().join(" → ");
        eprintln!(
            "{} {} → {} {}",
            "●".green(),
            "Agent turn".bold(),
            "fallback chain".cyan(),
            format!("[{}]", labels).dimmed()
        );
        Box::new(fb)
    } else if let Some(ref key) = opts.api_key {
        let url = opts
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.moonshot.ai/v1".to_string());
        let model = opts.model.clone().unwrap_or_else(|| "kimi-k2.5".to_string());
        eprintln!(
            "{} {} → {} {}",
            "●".green(),
            "Agent turn".bold(),
            model.cyan(),
            format!("({})", url).dimmed()
        );
        Box::new(OpenAiCompatibleProvider::new(&url, key, &model))
    } else {
        let (base_url, api_key, default_model) = resolve_provider_from_config()?;
        let model = opts.model.clone().unwrap_or(default_model);
        eprintln!(
            "{} {} → {} {}",
            "●".green(),
            "Agent turn".bold(),
            model.cyan(),
            format!("({})", base_url).dimmed()
        );
        Box::new(OpenAiCompatibleProvider::new(&base_url, &api_key, &model))
    };

    // ── Session handling (Postgres) ──
    // Initialize Postgres if available
    let _ = openclaw_db::try_init().await;
    let pool = openclaw_db::pool()
        .ok_or_else(|| anyhow::anyhow!("Postgres not available — set DATABASE_URL"))?;

    let session_key = if opts.continue_session {
        match openclaw_db::sessions::find_latest_session(pool, &format!("agent:{}:", opts.agent)).await? {
            Some(key) => {
                eprintln!("  {} {}", "Continuing session:".dimmed(), key.dimmed());
                key
            }
            None => {
                let key = new_session_key(&opts.agent);
                eprintln!("  {} (no previous session)", "New session".dimmed());
                key
            }
        }
    } else if let Some(ref key) = opts.session {
        key.clone()
    } else {
        new_session_key(&opts.agent)
    };

    openclaw_db::sessions::upsert_session(
        pool, &session_key, &opts.agent, provider.name(), Some("cli"), None,
    ).await?;

    // ── Load prior messages if continuing ──
    let _prior_messages = if opts.continue_session || opts.session.is_some() {
        let msgs = openclaw_db::sessions::load_messages(pool, &session_key).await?;
        if !msgs.is_empty() {
            eprintln!(
                "  {} {} prior messages loaded",
                "↻".dimmed(),
                msgs.len()
            );
        }
        msgs
    } else {
        Vec::new()
    };

    // ── Set up tools and config ──
    let tools = ToolRegistry::with_defaults();
    let config = AgentTurnConfig {
        agent_name: opts.agent.clone(),
        session_key: session_key.clone(),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
    ..AgentTurnConfig::default()
    };

    // ── Run agent turn ──
    let result = runtime::run_agent_turn(
        provider.as_ref(),
        &opts.message,
        &config,
        &tools,
    )
    .await?;

    // ── Print response ──
    if !result.response.is_empty() {
        println!("{}", result.response);
    } else if let Some(ref reasoning) = result.reasoning {
        println!("{}", reasoning);
    }

    // ── Persist messages to Postgres ──
    if let Ok(Some(sid)) = openclaw_db::sessions::get_session_id(pool, &session_key).await {
        let _ = openclaw_db::messages::record_message(
            pool, sid, "user", Some(&opts.message), None, None, None,
        ).await;
        if !result.response.is_empty() {
            let _ = openclaw_db::messages::record_message(
                pool, sid, "assistant", Some(&result.response), None, None, None,
            ).await;
        }
        let _ = openclaw_db::sessions::add_tokens(pool, &session_key,
            result.total_usage.total_tokens as i64).await;
    }

    // ── Print stats ──
    let total_ms = t_start.elapsed().as_millis();
    eprintln!();
    eprintln!("{}", "─".repeat(50).dimmed());
    eprintln!(
        "  {} {:.0}ms  {} {} round(s), {} tool call(s)",
        "Time:".dimmed(),
        total_ms,
        "│".dimmed(),
        result.total_rounds,
        result.tool_calls_made,
    );
    eprintln!(
        "  {} {} prompt + {} completion = {} total",
        "Tokens:".dimmed(),
        result.total_usage.prompt_tokens,
        result.total_usage.completion_tokens,
        result.total_usage.total_tokens,
    );
    eprintln!(
        "  {} {} / {} / {}",
        "Model:".dimmed(),
        provider.name().cyan(),
        opts.agent.dimmed(),
        session_key.dimmed()
    );

    Ok(())
}

fn resolve_provider_from_config() -> Result<(String, String, String)> {
    let config_path = openclaw_core::paths::manual_config_path();
    if !config_path.exists() {
        anyhow::bail!("Config not found at {}", config_path.display());
    }

    let content = std::fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    let providers = config
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("No model providers configured"))?;

    let preferred_order = ["moonshot", "openai-compatible"];

    for &provider_name in &preferred_order {
        if let Some(provider) = providers.get(provider_name) {
            if let (Some(base_url), Some(api_key)) = (
                provider.get("baseUrl").and_then(|v| v.as_str()),
                provider.get("apiKey").and_then(|v| v.as_str()),
            ) {
                let model = provider
                    .get("models")
                    .and_then(|m| m.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|m| m.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("kimi-k2.5");

                return Ok((
                    base_url.to_string(),
                    api_key.to_string(),
                    model.to_string(),
                ));
            }
        }
    }

    for (_name, provider) in providers {
        if let (Some(base_url), Some(api_key)) = (
            provider.get("baseUrl").and_then(|v| v.as_str()),
            provider.get("apiKey").and_then(|v| v.as_str()),
        ) {
            let model = provider
                .get("models")
                .and_then(|m| m.as_array())
                .and_then(|arr| arr.first())
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("default");

            return Ok((
                base_url.to_string(),
                api_key.to_string(),
                model.to_string(),
            ));
        }
    }

    anyhow::bail!("No provider with API key found in config")
}

fn new_session_key(agent: &str) -> String {
    format!("agent:{}:cli:{}", agent, uuid::Uuid::new_v4())
}
