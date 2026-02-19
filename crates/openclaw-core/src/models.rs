use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A configured LLM provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    pub base_url: String,
    pub api: String,
    pub model_count: usize,
    pub models: Vec<ModelInfo>,
}

/// A single model within a provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub reasoning: bool,
    pub context_window: Option<u64>,
    pub local: bool,
}

/// Load all providers and their models from openclaw-manual.json
pub fn load_providers(config_path: &Path) -> Result<Vec<ProviderInfo>> {
    let content = std::fs::read_to_string(config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    let providers = config
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("No model providers in config"))?;

    let mut result = Vec::new();

    for (name, provider) in providers {
        let base_url = provider
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let api = provider
            .get("api")
            .and_then(|v| v.as_str())
            .unwrap_or("openai-completions")
            .to_string();
        let is_local = api == "ollama" || base_url.contains("localhost") || base_url.contains("127.0.0.1");

        let models: Vec<ModelInfo> = provider
            .get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let id = m.get("id").and_then(|v| v.as_str())?;
                        let display_name = m
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(id)
                            .to_string();
                        let reasoning = m
                            .get("reasoning")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let context_window = m
                            .get("contextWindow")
                            .and_then(|v| v.as_u64());
                        Some(ModelInfo {
                            id: id.to_string(),
                            display_name,
                            reasoning,
                            context_window,
                            local: is_local,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let model_count = models.len();
        result.push(ProviderInfo {
            name: name.clone(),
            base_url,
            api,
            model_count,
            models,
        });
    }

    // Sort: local providers first, then alphabetical
    result.sort_by(|a, b| {
        let a_local = a.models.first().map(|m| m.local).unwrap_or(false);
        let b_local = b.models.first().map(|m| m.local).unwrap_or(false);
        b_local.cmp(&a_local).then(a.name.cmp(&b.name))
    });

    Ok(result)
}

/// Load the fallback chain order from config
pub fn load_fallback_chain(config_path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    if let Some(fallbacks) = config
        .get("models")
        .and_then(|m| m.get("fallbacks"))
        .and_then(|f| f.as_array())
    {
        Ok(fallbacks
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_providers_from_json() {
        let json = r#"{
            "models": {
                "providers": {
                    "ollama": {
                        "baseUrl": "http://127.0.0.1:11434",
                        "api": "ollama",
                        "models": [
                            {"id": "llama3.2:1b", "name": "Llama 3.2 1B", "contextWindow": 131072}
                        ]
                    },
                    "moonshot": {
                        "baseUrl": "https://api.moonshot.ai/v1",
                        "api": "openai-completions",
                        "models": [
                            {"id": "kimi-k2.5", "name": "Kimi K2.5", "reasoning": true}
                        ]
                    }
                }
            }
        }"#;

        let tmp = std::env::temp_dir().join("openclaw-test-models.json");
        std::fs::write(&tmp, json).unwrap();

        let providers = load_providers(&tmp).unwrap();
        assert_eq!(providers.len(), 2);

        // ollama should be first (local)
        assert_eq!(providers[0].name, "ollama");
        assert_eq!(providers[0].models.len(), 1);
        assert!(providers[0].models[0].local);
        assert_eq!(providers[0].models[0].context_window, Some(131072));

        // moonshot second
        assert_eq!(providers[1].name, "moonshot");
        assert!(providers[1].models[0].reasoning);
        assert!(!providers[1].models[0].local);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_parse_fallback_chain() {
        let json = r#"{
            "models": {
                "fallbacks": ["ollama/llama3.2:1b", "moonshot/kimi-k2.5"],
                "providers": {}
            }
        }"#;

        let tmp = std::env::temp_dir().join("openclaw-test-fallbacks.json");
        std::fs::write(&tmp, json).unwrap();

        let chain = load_fallback_chain(&tmp).unwrap();
        assert_eq!(chain, vec!["ollama/llama3.2:1b", "moonshot/kimi-k2.5"]);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_no_fallback_chain() {
        let json = r#"{"models": {"providers": {}}}"#;
        let tmp = std::env::temp_dir().join("openclaw-test-no-fb.json");
        std::fs::write(&tmp, json).unwrap();

        let chain = load_fallback_chain(&tmp).unwrap();
        assert!(chain.is_empty());

        std::fs::remove_file(&tmp).ok();
    }
}
