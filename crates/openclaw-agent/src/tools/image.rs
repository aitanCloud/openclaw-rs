use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};

pub struct ImageTool;

#[async_trait]
impl Tool for ImageTool {
    fn name(&self) -> &str {
        "image"
    }

    fn description(&self) -> &str {
        "Analyze an image using a vision-capable LLM. Provide a URL (http/https or local file path) and a question about the image."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Image URL (http/https) or absolute file path to analyze"
                },
                "question": {
                    "type": "string",
                    "description": "What to analyze or ask about the image (default: 'Describe this image in detail')"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return Ok(ToolResult::error("image: missing 'url' argument")),
        };

        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image in detail");

        // Resolve image to a data URL
        let data_url = if url.starts_with("http://") || url.starts_with("https://") {
            // Fetch remote image and base64 encode
            match fetch_and_encode(&url).await {
                Ok(du) => du,
                Err(e) => return Ok(ToolResult::error(format!("Failed to fetch image: {}", e))),
            }
        } else {
            // Local file — resolve relative to workspace
            let path = if url.starts_with('/') {
                std::path::PathBuf::from(&url)
            } else {
                std::path::PathBuf::from(&ctx.workspace_dir).join(&url)
            };

            if !path.exists() {
                return Ok(ToolResult::error(format!("File not found: {}", path.display())));
            }

            match encode_local_file(&path) {
                Ok(du) => du,
                Err(e) => return Ok(ToolResult::error(format!("Failed to read image: {}", e))),
            }
        };

        // Make a one-shot vision call
        match vision_query(&data_url, question).await {
            Ok(response) => Ok(ToolResult::success(response)),
            Err(e) => Ok(ToolResult::error(format!("Vision analysis failed: {}", e))),
        }
    }
}

async fn fetch_and_encode(url: &str) -> Result<String> {
    use base64::Engine;

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "openclaw-agent/0.1")
        .send()
        .await?;

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    let mime = if content_type.contains("png") {
        "image/png"
    } else if content_type.contains("gif") {
        "image/gif"
    } else if content_type.contains("webp") {
        "image/webp"
    } else {
        "image/jpeg"
    };

    let bytes = response.bytes().await?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

fn encode_local_file(path: &std::path::Path) -> Result<String> {
    use base64::Engine;

    let bytes = std::fs::read(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg")
        .to_lowercase();

    let mime = match ext.as_str() {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/jpeg",
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{};base64,{}", mime, b64))
}

/// Make a one-shot vision query using the first available provider from env
async fn vision_query(image_data_url: &str, question: &str) -> Result<String> {
    // Try provider env vars in priority order
    let (base_url, api_key, model) = resolve_vision_provider()?;

    let messages = serde_json::json!([
        {
            "role": "user",
            "content": [
                {"type": "text", "text": question},
                {"type": "image_url", "image_url": {"url": image_data_url}}
            ]
        }
    ]);

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": 1024,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/chat/completions", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let err = response.text().await.unwrap_or_default();
        anyhow::bail!("Vision API returned {}: {}", status, err);
    }

    let resp: VisionResponse = response.json().await?;
    let content = resp
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .unwrap_or_else(|| "(no response)".to_string());

    Ok(content)
}

fn resolve_vision_provider() -> Result<(String, String, String)> {
    // Check for dedicated OPENCLAW_VISION_* env vars first
    if let (Ok(url), Ok(key), Ok(model)) = (
        std::env::var("OPENCLAW_VISION_BASE_URL"),
        std::env::var("OPENCLAW_VISION_API_KEY"),
        std::env::var("OPENCLAW_VISION_MODEL"),
    ) {
        return Ok((url, key, model));
    }

    // Fall back to OpenAI
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("OPENAI_MODEL")
            .unwrap_or_else(|_| "gpt-4o".to_string());
        return Ok((url, key, model));
    }

    // Fall back to any configured provider
    for prefix in &["OPENROUTER", "DEEPSEEK", "ANTHROPIC", "GROQ"] {
        let key_var = format!("{}_API_KEY", prefix);
        let url_var = format!("{}_BASE_URL", prefix);
        let model_var = format!("{}_MODEL", prefix);

        if let Ok(key) = std::env::var(&key_var) {
            let url = std::env::var(&url_var)
                .unwrap_or_else(|_| default_base_url(prefix));
            let model = std::env::var(&model_var)
                .unwrap_or_else(|_| default_vision_model(prefix));
            return Ok((url, key, model));
        }
    }

    anyhow::bail!(
        "No vision provider configured. Set OPENCLAW_VISION_BASE_URL/API_KEY/MODEL or OPENAI_API_KEY"
    )
}

fn default_base_url(prefix: &str) -> String {
    match prefix {
        "OPENROUTER" => "https://openrouter.ai/api/v1".to_string(),
        "DEEPSEEK" => "https://api.deepseek.com/v1".to_string(),
        "ANTHROPIC" => "https://api.anthropic.com/v1".to_string(),
        "GROQ" => "https://api.groq.com/openai/v1".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
    }
}

fn default_vision_model(prefix: &str) -> String {
    match prefix {
        "OPENROUTER" => "openai/gpt-4o".to_string(),
        "ANTHROPIC" => "claude-sonnet-4-20250514".to_string(),
        _ => "gpt-4o".to_string(),
    }
}

#[derive(Deserialize)]
struct VisionResponse {
    choices: Vec<VisionChoice>,
}

#[derive(Deserialize)]
struct VisionChoice {
    message: VisionMessage,
}

#[derive(Deserialize)]
struct VisionMessage {
    #[serde(default)]
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_local_file_missing() {
        let result = encode_local_file(std::path::Path::new("/tmp/nonexistent_image_12345.png"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_image_tool_missing_url() {
        let tool = ImageTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };
        let args = serde_json::json!({"question": "what is this?"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[tokio::test]
    async fn test_image_tool_file_not_found() {
        let tool = ImageTool;
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };
        let args = serde_json::json!({"url": "/tmp/nonexistent_image_12345.png"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }
}
