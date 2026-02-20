//! Subagent system — allows the main agent to delegate subtasks to isolated agent turns.
//!
//! A subagent runs a single agent turn with its own message history (just the prompt),
//! using the same LLM provider and tools as the parent. The subagent inherits the
//! parent's cancellation token if one is active.

use anyhow::Result;
use tracing::{info, warn};

use crate::llm::fallback::FallbackProvider;
use crate::llm::streaming::StreamEvent;
use crate::llm::{LlmProvider, OpenAiCompatibleProvider};
use crate::runtime::{run_agent_turn_streaming, AgentTurnConfig};
use crate::tools::ToolRegistry;

/// Run a subagent turn with the given prompt.
/// Uses the same provider config as the parent agent but with a fresh message history.
/// Returns the subagent's text response.
pub async fn run_subagent_turn(
    prompt: &str,
    agent_name: &str,
    workspace_dir: &str,
    cancel_token: Option<tokio_util::sync::CancellationToken>,
) -> Result<String> {
    info!("Starting subagent turn for agent={}", agent_name);

    // Build provider from config (same as parent)
    let provider = build_provider_from_config()?;

    let config = AgentTurnConfig {
        agent_name: agent_name.to_string(),
        session_key: format!("subagent:{}:{}", agent_name, uuid::Uuid::new_v4()),
        workspace_dir: workspace_dir.to_string(),
        minimal_context: true,
        ..AgentTurnConfig::default()
    };

    let mut tools = ToolRegistry::with_defaults();
    let workspace_path = std::path::Path::new(workspace_dir);
    tools.load_plugins(workspace_path);

    // Remove the delegate tool from subagent to prevent infinite recursion
    // (subagents cannot spawn further subagents)
    let tools = ToolRegistry::without_tool(tools, "delegate");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

    // Drain events in background (subagent events are not streamed to user)
    let drain_handle = tokio::spawn(async move {
        while event_rx.recv().await.is_some() {}
    });

    let subagent_preamble = "[Subagent Context] You are running as a focused subagent. \
         Complete the task efficiently and return a clear result. \
         Do NOT busy-poll or repeatedly check status — execute your work and finish. \
         If a command fails, try a different approach rather than retrying the same command.";

    let prompt = format!("{}\n\n[Subagent Task]: {}", subagent_preamble, prompt);

    let result = run_agent_turn_streaming(
        &*provider,
        &prompt,
        &config,
        &tools,
        event_tx,
        Vec::new(), // no images for subagent
        cancel_token, // propagate parent's cancellation token
    )
    .await?;

    drain_handle.abort();

    info!(
        "Subagent completed: {}ms, {} tokens",
        result.elapsed_ms, result.total_usage.total_tokens
    );

    Ok(result.response)
}

/// Build an LLM provider from the openclaw-manual.json config.
/// Tries fallback chain first, falls back to single model.
fn build_provider_from_config() -> Result<Box<dyn LlmProvider>> {
    match FallbackProvider::from_config() {
        Ok(fb) => Ok(Box::new(fb)),
        Err(e) => {
            warn!("Fallback provider unavailable for subagent: {}", e);
            // Try to build a single provider from env
            let api_key = std::env::var("OPENAI_API_KEY")
                .or_else(|_| std::env::var("MOONSHOT_API_KEY"))
                .or_else(|_| std::env::var("OLLAMA_API_KEY"))
                .unwrap_or_else(|_| "ollama-local".to_string());

            let base_url = std::env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());

            let model = std::env::var("MODEL")
                .unwrap_or_else(|_| "llama3.2:1b".to_string());

            Ok(Box::new(OpenAiCompatibleProvider::new(
                &base_url, &api_key, &model,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_subagent_session_key_format() {
        let key = format!("subagent:{}:{}", "main", uuid::Uuid::new_v4());
        assert!(key.starts_with("subagent:main:"));
        assert!(key.len() > 20); // UUID adds significant length
    }
}
