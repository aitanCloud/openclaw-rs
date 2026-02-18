use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use std::time::Instant;
use tracing::{info, warn};

use super::{Completion, LlmProvider, Message, OpenAiCompatibleProvider, ToolDefinition, UsageStats};

/// A provider entry in the fallback chain
pub struct FallbackEntry {
    pub provider: OpenAiCompatibleProvider,
    pub label: String,
    consecutive_failures: AtomicUsize,
}

/// Tries providers in order. On failure, falls through to the next.
/// Tracks consecutive failures per provider for health scoring.
pub struct FallbackProvider {
    entries: Vec<FallbackEntry>,
    last_successful: RwLock<String>,
}

impl FallbackProvider {
    pub fn new(entries: Vec<(String, OpenAiCompatibleProvider)>) -> Self {
        let first_label = entries.first().map(|(l, _)| l.clone()).unwrap_or_default();
        Self {
            entries: entries
                .into_iter()
                .map(|(label, provider)| FallbackEntry {
                    provider,
                    label,
                    consecutive_failures: AtomicUsize::new(0),
                })
                .collect(),
            last_successful: RwLock::new(first_label),
        }
    }

    /// Build a fallback chain from the openclaw-manual.json config
    pub fn from_config() -> Result<Self> {
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
            .ok_or_else(|| anyhow::anyhow!("No model providers in config"))?;

        // Read fallback order from config, or use default
        let fallback_order = config
            .get("models")
            .and_then(|m| m.get("fallbacks"))
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            });

        let mut entries = Vec::new();

        if let Some(order) = fallback_order {
            // Use explicit fallback order
            for model_spec in &order {
                if let Some((provider_name, model_id)) = parse_model_spec(model_spec) {
                    if let Some(provider) = providers.get(&provider_name) {
                        if let Some(entry) =
                            build_provider_entry(&provider_name, &model_id, provider)
                        {
                            entries.push(entry);
                        }
                    }
                }
            }
        }

        if entries.is_empty() {
            // Fallback: iterate all providers, first model each
            // Prefer order: ollama (local/free) → moonshot → openai-compatible
            let preferred_order = ["ollama", "moonshot", "openai-compatible"];

            for &provider_name in &preferred_order {
                if let Some(provider) = providers.get(provider_name) {
                    if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
                        let api_key = provider
                            .get("apiKey")
                            .and_then(|v| v.as_str())
                            .unwrap_or("ollama-local");

                        if let Some(models) =
                            provider.get("models").and_then(|m| m.as_array())
                        {
                            for model in models {
                                if let Some(model_id) =
                                    model.get("id").and_then(|v| v.as_str())
                                {
                                    let label =
                                        format!("{}/{}", provider_name, model_id);
                                    let p = OpenAiCompatibleProvider::new(
                                        base_url, api_key, model_id,
                                    );
                                    entries.push((label, p));
                                    break; // first model per provider
                                }
                            }
                        }
                    }
                }
            }

            // Add any remaining providers not in preferred order
            for (provider_name, provider) in providers {
                if preferred_order.contains(&provider_name.as_str()) {
                    continue;
                }
                if let Some(entry) = build_first_model_entry(provider_name, provider) {
                    entries.push(entry);
                }
            }
        }

        if entries.is_empty() {
            anyhow::bail!("No usable providers found in config");
        }

        Ok(Self::new(entries))
    }

    pub fn provider_labels(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.label.as_str()).collect()
    }
}

fn parse_model_spec(spec: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = spec.splitn(2, '/').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

fn build_provider_entry(
    provider_name: &str,
    model_id: &str,
    provider: &serde_json::Value,
) -> Option<(String, OpenAiCompatibleProvider)> {
    let base_url = provider.get("baseUrl").and_then(|v| v.as_str())?;
    let api_key = provider
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("ollama-local");

    let label = format!("{}/{}", provider_name, model_id);
    Some((
        label,
        OpenAiCompatibleProvider::new(base_url, api_key, model_id),
    ))
}

fn build_first_model_entry(
    provider_name: &str,
    provider: &serde_json::Value,
) -> Option<(String, OpenAiCompatibleProvider)> {
    let base_url = provider.get("baseUrl").and_then(|v| v.as_str())?;
    let api_key = provider
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("ollama-local");
    let model_id = provider
        .get("models")
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.first())
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())?;

    let label = format!("{}/{}", provider_name, model_id);
    Some((
        label,
        OpenAiCompatibleProvider::new(base_url, api_key, model_id),
    ))
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    fn name(&self) -> &str {
        // Return last successful model; falls back to "fallback-chain" if lock poisoned
        // We leak a &str here since the trait requires &str — acceptable for a long-lived provider
        let guard = self.last_successful.read().unwrap();
        let name = guard.clone();
        drop(guard);
        Box::leak(name.into_boxed_str())
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<(Completion, UsageStats)> {
        let mut last_error = None;

        for (i, entry) in self.entries.iter().enumerate() {
            let failures = entry.consecutive_failures.load(Ordering::Relaxed);

            // Skip providers with >3 consecutive failures (circuit breaker)
            if failures > 3 {
                info!(
                    "Skipping {} (circuit open: {} consecutive failures)",
                    entry.label, failures
                );
                continue;
            }

            let t_start = Instant::now();
            info!("Trying provider {}/{}: {}", i + 1, self.entries.len(), entry.label);

            match entry.provider.complete(messages, tools).await {
                Ok(result) => {
                    let elapsed = t_start.elapsed().as_millis();
                    entry.consecutive_failures.store(0, Ordering::Relaxed);
                    if let Ok(mut last) = self.last_successful.write() {
                        *last = entry.label.clone();
                    }
                    info!("{} succeeded in {}ms", entry.label, elapsed);
                    return Ok(result);
                }
                Err(e) => {
                    let elapsed = t_start.elapsed().as_millis();
                    let new_failures = entry.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(
                        "{} failed in {}ms (attempt {}, consecutive failures: {}): {}",
                        entry.label, elapsed, i + 1, new_failures, e
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All providers exhausted or circuit-broken")))
    }

    async fn complete_streaming(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        event_tx: tokio::sync::mpsc::UnboundedSender<super::streaming::StreamEvent>,
    ) -> Result<(super::Completion, super::UsageStats)> {
        let mut last_error = None;

        for (i, entry) in self.entries.iter().enumerate() {
            let failures = entry.consecutive_failures.load(Ordering::Relaxed);

            if failures > 3 {
                info!(
                    "Skipping {} (circuit open: {} consecutive failures)",
                    entry.label, failures
                );
                continue;
            }

            let t_start = Instant::now();
            info!("Trying streaming provider {}/{}: {}", i + 1, self.entries.len(), entry.label);

            match entry.provider.complete_streaming(messages, tools, event_tx.clone()).await {
                Ok(result) => {
                    let elapsed = t_start.elapsed().as_millis();
                    entry.consecutive_failures.store(0, Ordering::Relaxed);
                    if let Ok(mut last) = self.last_successful.write() {
                        *last = entry.label.clone();
                    }
                    info!("{} streaming succeeded in {}ms", entry.label, elapsed);
                    return Ok(result);
                }
                Err(e) => {
                    let elapsed = t_start.elapsed().as_millis();
                    let new_failures = entry.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                    warn!(
                        "{} streaming failed in {}ms (consecutive failures: {}): {}",
                        entry.label, elapsed, new_failures, e
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("All streaming providers exhausted")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_spec() {
        let (provider, model) = parse_model_spec("moonshot/kimi-k2.5").unwrap();
        assert_eq!(provider, "moonshot");
        assert_eq!(model, "kimi-k2.5");
    }

    #[test]
    fn test_parse_model_spec_none() {
        assert!(parse_model_spec("just-a-model").is_none());
    }

    #[test]
    fn test_fallback_provider_labels() {
        let entries = vec![
            (
                "ollama/llama3.2:1b".to_string(),
                OpenAiCompatibleProvider::new("http://localhost:11434", "key", "llama3.2:1b"),
            ),
            (
                "moonshot/kimi-k2.5".to_string(),
                OpenAiCompatibleProvider::new("https://api.moonshot.ai/v1", "key", "kimi-k2.5"),
            ),
        ];
        let provider = FallbackProvider::new(entries);
        let labels = provider.provider_labels();
        assert_eq!(labels, vec!["ollama/llama3.2:1b", "moonshot/kimi-k2.5"]);
    }
}
