use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

pub struct TtsTool;

/// Default Piper model path
const DEFAULT_MODEL_DIR: &str = ".openclaw/projects/voice-control/models";
const DEFAULT_MODEL: &str = "en_GB-jenny_dioco-medium.onnx";

#[async_trait]
impl Tool for TtsTool {
    fn name(&self) -> &str {
        "tts"
    }

    fn description(&self) -> &str {
        "Convert text to speech audio using Piper TTS. Returns the path to the generated WAV file. The name 'AItan' should be written as 'Ay-tawn' for correct pronunciation."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to convert to speech"
                },
                "output": {
                    "type": "string",
                    "description": "Output file path (optional, defaults to auto-generated path in workspace)"
                },
                "model": {
                    "type": "string",
                    "description": "Piper model name (optional, defaults to jenny_dioco medium)"
                },
                "speaker": {
                    "type": "integer",
                    "description": "Speaker ID for multi-speaker models (optional)"
                },
                "length_scale": {
                    "type": "number",
                    "description": "Speech speed: <1.0 = faster, >1.0 = slower (optional, default 1.0)"
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let text = match args.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(ToolResult::error("tts: missing or empty 'text' argument")),
        };

        // Resolve model path
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let model_dir = home.join(DEFAULT_MODEL_DIR);

        let model_name = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_MODEL);

        let model_path = model_dir.join(model_name);
        if !model_path.exists() {
            return Ok(ToolResult::error(format!(
                "Piper model not found: {}. Available models in {}",
                model_path.display(),
                model_dir.display()
            )));
        }

        // Resolve output path
        let output_path = match args.get("output").and_then(|v| v.as_str()) {
            Some(p) => PathBuf::from(p),
            None => {
                let tts_dir = PathBuf::from(&ctx.workspace_dir).join("tts-output");
                std::fs::create_dir_all(&tts_dir)?;
                let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                tts_dir.join(format!("tts_{}.wav", ts))
            }
        };

        // Build piper command
        let mut cmd = tokio::process::Command::new("piper");
        cmd.arg("--model").arg(&model_path);
        cmd.arg("--output_file").arg(&output_path);

        if let Some(speaker) = args.get("speaker").and_then(|v| v.as_i64()) {
            cmd.arg("--speaker").arg(speaker.to_string());
        }

        if let Some(length_scale) = args.get("length_scale").and_then(|v| v.as_f64()) {
            cmd.arg("--length-scale").arg(length_scale.to_string());
        }

        // Pipe text via stdin
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("piper not found in PATH. Install piper-tts first.")
            } else {
                anyhow::anyhow!("Failed to spawn piper: {}", e)
            }
        })?;

        // Write text to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(text.as_bytes()).await?;
            drop(stdin);
        }

        // Wait with timeout
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            child.wait_with_output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                if output.status.success() {
                    let file_size = std::fs::metadata(&output_path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    Ok(ToolResult::success(format!(
                        "Generated speech audio: {} ({} bytes, {} chars of text)",
                        output_path.display(),
                        file_size,
                        text.len()
                    )))
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Ok(ToolResult::error(format!(
                        "Piper exited with {}: {}",
                        output.status,
                        stderr.trim()
                    )))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("Piper process error: {}", e))),
            Err(_) => Ok(ToolResult::error("Piper timed out after 30s")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_dir: "/tmp/tts-test".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        }
    }

    #[tokio::test]
    async fn test_tts_missing_text() {
        let tool = TtsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[tokio::test]
    async fn test_tts_empty_text() {
        let tool = TtsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"text": ""});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_tts_bad_model() {
        let tool = TtsTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"text": "hello", "model": "nonexistent-model.onnx"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[test]
    fn test_tool_metadata() {
        let tool = TtsTool;
        assert_eq!(tool.name(), "tts");
        assert!(tool.description().contains("speech"));
        let params = tool.parameters();
        assert!(params.get("required").is_some());
    }
}
