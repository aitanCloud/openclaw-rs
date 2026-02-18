use anyhow::Result;
use colored::Colorize;
use openclaw_agent::llm::OpenAiCompatibleProvider;
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
}

/// Resolve LLM provider config from openclaw-manual.json
fn resolve_provider_from_config() -> Result<(String, String, String)> {
    let config_path = openclaw_core::paths::manual_config_path();
    if !config_path.exists() {
        anyhow::bail!("Config not found at {}", config_path.display());
    }

    let content = std::fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    // Walk providers to find one with an API key
    let providers = config
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("No model providers configured"))?;

    // Prefer moonshot, then openai-compatible, then first available
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

    // Fallback: first provider with an API key
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

pub async fn run(opts: AgentOptions) -> Result<()> {
    let t_start = Instant::now();

    // Resolve provider
    let (base_url, api_key, default_model) = if let Some(ref key) = opts.api_key {
        let url = opts
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.moonshot.ai/v1".to_string());
        (url, key.clone(), "kimi-k2.5".to_string())
    } else {
        resolve_provider_from_config()?
    };

    let model = opts.model.unwrap_or(default_model);
    let workspace_dir = workspace::resolve_workspace_dir(&opts.agent);

    if !workspace_dir.exists() {
        anyhow::bail!(
            "Workspace not found: {}. Run 'openclaw' (Node.js) first to initialize.",
            workspace_dir.display()
        );
    }

    // Set up provider and tools
    let provider = OpenAiCompatibleProvider::new(&base_url, &api_key, &model);
    let tools = ToolRegistry::with_defaults();

    let config = AgentTurnConfig {
        agent_name: opts.agent.clone(),
        session_key: format!("agent:{}:cli:{}", opts.agent, uuid_v4()),
        workspace_dir: workspace_dir.to_string_lossy().to_string(),
        minimal_context: false,
    };

    eprintln!(
        "{} {} → {} {}",
        "●".green(),
        "Agent turn".bold(),
        model.cyan(),
        format!("({})", base_url).dimmed()
    );

    let result = runtime::run_agent_turn(&provider, &opts.message, &config, &tools).await?;

    // Print response
    if !result.response.is_empty() {
        println!("{}", result.response);
    } else if let Some(ref reasoning) = result.reasoning {
        // Reasoning-only model (like kimi-k2.5 sometimes)
        println!("{}", reasoning);
    }

    // Print stats
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
        "  {} {} / {}",
        "Model:".dimmed(),
        model.cyan(),
        opts.agent.dimmed()
    );

    Ok(())
}

/// Simple UUID v4 generator (no external dep needed)
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:032x}", t)
}
