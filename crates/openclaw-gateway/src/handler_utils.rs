//! Shared utilities for Telegram and Discord message handlers.

use anyhow::Result;
use openclaw_agent::llm::OpenAiCompatibleProvider;

/// Resolve a single LLM provider by model spec (e.g. "moonshot/kimi-k2.5")
pub fn resolve_single_provider(model_spec: &str) -> Result<OpenAiCompatibleProvider> {
    let config_path = openclaw_core::paths::manual_config_path();
    let content = std::fs::read_to_string(&config_path)?;
    let config: serde_json::Value = serde_json::from_str(&content)?;

    let providers = config
        .get("models")
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.as_object())
        .ok_or_else(|| anyhow::anyhow!("No providers in config"))?;

    for (pname, provider) in providers {
        if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
            let api_key = provider
                .get("apiKey")
                .and_then(|v| v.as_str())
                .unwrap_or("ollama-local");
            if let Some(models) = provider.get("models").and_then(|m| m.as_array()) {
                for model in models {
                    if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                        if id == model_spec || format!("{}/{}", pname, id) == model_spec {
                            return Ok(OpenAiCompatibleProvider::new(base_url, api_key, id));
                        }
                    }
                }
            }
        }
    }

    anyhow::bail!("Model '{}' not found in any provider", model_spec)
}

/// Toggle a cron job's enabled state by name (case-insensitive partial match)
pub fn toggle_cron_job(path: &std::path::Path, name: &str, enable: bool) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    let mut cron_file: serde_json::Value = serde_json::from_str(&content)?;

    let jobs = cron_file
        .get_mut("jobs")
        .and_then(|j| j.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("Invalid cron jobs file"))?;

    let name_lower = name.to_lowercase();
    let mut found = None;

    for job in jobs.iter_mut() {
        let job_name = job
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if job_name.to_lowercase().contains(&name_lower) {
            job.as_object_mut()
                .unwrap()
                .insert("enabled".to_string(), serde_json::json!(enable));
            found = Some(job_name);
            break;
        }
    }

    match found {
        Some(job_name) => {
            let updated = serde_json::to_string_pretty(&cron_file)?;
            std::fs::write(path, updated)?;
            Ok(job_name)
        }
        None => Err(anyhow::anyhow!("No cron job matching '{}' found", name)),
    }
}

/// Format a duration in milliseconds as a human-readable "Xs ago" / "Xm ago" / "Xh ago" / "Xd ago"
pub fn format_duration(ms: i64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Split a long message into chunks at newline boundaries, respecting a max length per chunk
pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }
        let split_at = remaining[..max_len].rfind('\n').unwrap_or(max_len);
        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start_matches('\n');
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(5_000), "5s ago");
        assert_eq!(format_duration(59_000), "59s ago");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60_000), "1m ago");
        assert_eq!(format_duration(3_599_000), "59m ago");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3_600_000), "1h ago");
        assert_eq!(format_duration(86_399_000), "23h ago");
    }

    #[test]
    fn test_format_duration_days() {
        assert_eq!(format_duration(86_400_000), "1d ago");
        assert_eq!(format_duration(172_800_000), "2d ago");
    }

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_message_long() {
        let long = "a\n".repeat(1500);
        let chunks = split_message(&long, 2000);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000, "Chunk too long: {}", chunk.len());
        }
        // All content should be preserved
        let rejoined: String = chunks.join("\n");
        assert!(rejoined.len() >= long.len() - chunks.len()); // approximate
    }

    #[test]
    fn test_split_message_exact_boundary() {
        let text = "x".repeat(2000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2000);
    }
}
