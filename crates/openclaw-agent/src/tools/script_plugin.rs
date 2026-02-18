use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, warn};

use super::{Tool, ToolContext, ToolResult};

const PLUGIN_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// A tool defined by a JSON manifest in the plugins directory.
/// The manifest specifies a shell command that receives arguments as JSON on stdin.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    /// Shell command to execute. Receives tool arguments as JSON on stdin.
    /// Working directory is set to the workspace.
    pub command: String,
    /// Optional timeout override in seconds (default: 30)
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

pub struct ScriptPluginTool {
    manifest: PluginManifest,
}

impl ScriptPluginTool {
    pub fn from_manifest(manifest: PluginManifest) -> Self {
        Self { manifest }
    }
}

#[async_trait]
impl Tool for ScriptPluginTool {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn parameters(&self) -> Value {
        if self.manifest.parameters.is_null() {
            serde_json::json!({"type": "object", "properties": {}})
        } else {
            self.manifest.parameters.clone()
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let timeout = self
            .manifest
            .timeout_secs
            .unwrap_or(PLUGIN_TIMEOUT_SECS);
        let timeout = ctx.sandbox.clamp_timeout(timeout);

        // Check command against sandbox blocklist
        if let Some(blocked) = ctx.sandbox.is_command_blocked(&self.manifest.command) {
            return Ok(ToolResult::error(format!(
                "Plugin command blocked by sandbox: contains '{}'",
                blocked
            )));
        }

        let args_json = serde_json::to_string(&args).unwrap_or_default();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            async {
                let mut child = Command::new("sh")
                    .arg("-c")
                    .arg(&self.manifest.command)
                    .current_dir(&ctx.workspace_dir)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;

                // Write args as JSON to stdin
                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    stdin.write_all(args_json.as_bytes()).await.ok();
                    drop(stdin);
                }

                child.wait_with_output().await
            },
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(MAX_OUTPUT_BYTES);
                    stdout.push_str("\n... (output truncated)");
                }

                let mut result_text = String::new();
                if !stdout.is_empty() {
                    result_text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result_text.is_empty() {
                        result_text.push('\n');
                    }
                    result_text.push_str("[stderr] ");
                    result_text.push_str(&stderr);
                }
                if result_text.is_empty() {
                    result_text = format!("(exit code {})", exit_code);
                }

                if exit_code == 0 {
                    Ok(ToolResult::success(result_text))
                } else {
                    Ok(ToolResult::error(result_text))
                }
            }
            Ok(Err(e)) => Ok(ToolResult::error(format!("Plugin exec failed: {}", e))),
            Err(_) => Ok(ToolResult::error(format!(
                "Plugin timed out after {}s",
                timeout
            ))),
        }
    }
}

/// Load all plugin manifests from a directory.
/// Each .json file in the directory is treated as a plugin manifest.
pub fn load_plugins(plugins_dir: &Path) -> Vec<ScriptPluginTool> {
    if !plugins_dir.exists() || !plugins_dir.is_dir() {
        return Vec::new();
    }

    let mut plugins = Vec::new();

    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read plugins directory: {}", e);
            return Vec::new();
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<PluginManifest>(&content) {
                Ok(manifest) => {
                    debug!("Loaded plugin: {} from {}", manifest.name, path.display());
                    plugins.push(ScriptPluginTool::from_manifest(manifest));
                }
                Err(e) => {
                    warn!("Failed to parse plugin {}: {}", path.display(), e);
                }
            },
            Err(e) => {
                warn!("Failed to read plugin {}: {}", path.display(), e);
            }
        }
    }

    plugins
}

/// Resolve the plugins directory for a workspace
pub fn plugins_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(".openclaw").join("plugins")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let json = r#"{
            "name": "greet",
            "description": "Say hello",
            "command": "echo hello",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Name to greet"}
                }
            }
        }"#;

        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "greet");
        assert_eq!(manifest.command, "echo hello");
        assert!(manifest.timeout_secs.is_none());
    }

    #[test]
    fn test_parse_minimal_manifest() {
        let json = r#"{
            "name": "ping",
            "description": "Ping test",
            "command": "echo pong"
        }"#;

        let manifest: PluginManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.name, "ping");
        assert!(manifest.parameters.is_null());
    }

    #[tokio::test]
    async fn test_plugin_execution() {
        let manifest = PluginManifest {
            name: "echo_test".to_string(),
            description: "Test plugin".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            command: "cat".to_string(), // cat will echo stdin back
            timeout_secs: Some(5),
        };

        let tool = ScriptPluginTool::from_manifest(manifest);
        let ctx = ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        };

        let args = serde_json::json!({"message": "hello"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[test]
    fn test_load_plugins_empty() {
        let plugins = load_plugins(Path::new("/nonexistent_plugins_dir"));
        assert!(plugins.is_empty());
    }
}
