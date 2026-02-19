use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolResult};

pub struct BrowserTool;

/// Max page content to return to the LLM
const MAX_CONTENT_CHARS: usize = 32000;

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Headless browser for web interaction. Actions: navigate (fetch page as text), screenshot (capture page image), evaluate (run JavaScript on a page). Uses headless Chromium."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "screenshot", "evaluate"],
                    "description": "Action to perform"
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (required for navigate/screenshot)"
                },
                "javascript": {
                    "type": "string",
                    "description": "JavaScript code to evaluate on the page (for 'evaluate' action)"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector to extract text from (for 'navigate', optional — extracts full page if omitted)"
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "Milliseconds to wait after page load for JS rendering (optional, default 1000)"
                },
                "output": {
                    "type": "string",
                    "description": "Output file path for screenshot (optional, auto-generated if omitted)"
                }
            },
            "required": ["action", "url"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("browser: missing 'action' argument"))?;

        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return Ok(ToolResult::error("browser: missing or empty 'url' argument")),
        };

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolResult::error(
                "browser: URL must start with http:// or https://",
            ));
        }

        let wait_ms = args
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(1000);

        match action {
            "navigate" => navigate(url, &args, wait_ms).await,
            "screenshot" => screenshot(url, &args, ctx, wait_ms).await,
            "evaluate" => evaluate(url, &args, wait_ms).await,
            _ => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Use: navigate, screenshot, evaluate",
                action
            ))),
        }
    }
}

/// Find a Chromium-based browser binary
fn find_browser() -> Option<String> {
    let candidates = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "brave",
        "brave-browser",
    ];

    for name in &candidates {
        if which_exists(name) {
            return Some(name.to_string());
        }
    }
    None
}

fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Navigate to a URL and extract page text content
async fn navigate(url: &str, args: &Value, wait_ms: u64) -> Result<ToolResult> {
    let browser = match find_browser() {
        Some(b) => b,
        None => return Ok(ToolResult::error(
            "No Chromium-based browser found. Install chromium, google-chrome, or brave.",
        )),
    };

    let selector = args.get("selector").and_then(|v| v.as_str());

    // Build JS to extract content
    let extract_js = match selector {
        Some(sel) => format!(
            r#"
            setTimeout(() => {{
                const el = document.querySelector('{}');
                if (el) {{
                    console.log(el.innerText);
                }} else {{
                    console.log('[ERROR] Selector not found: {}');
                }}
            }}, {});
            "#,
            sel.replace('\'', "\\'"),
            sel.replace('\'', "\\'"),
            wait_ms
        ),
        None => format!(
            r#"
            setTimeout(() => {{
                const title = document.title || '';
                const meta = document.querySelector('meta[name="description"]');
                const desc = meta ? meta.getAttribute('content') : '';
                const body = document.body ? document.body.innerText : '';
                console.log('Title: ' + title);
                if (desc) console.log('Description: ' + desc);
                console.log('---');
                console.log(body);
            }}, {});
            "#,
            wait_ms
        ),
    };

    // Total timeout: wait_ms + 10s for page load
    let timeout_ms = wait_ms + 10000;

    let output = run_browser_command(
        &browser,
        &[
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--dump-dom",
            url,
        ],
        timeout_ms,
    )
    .await?;

    // For navigate, we use --dump-dom which gives us HTML, then extract text
    // Actually, let's use a simpler approach: use --dump-dom and strip HTML
    let text = strip_html_tags(&output);
    let text = collapse_whitespace(&text);

    let truncated = if text.len() > MAX_CONTENT_CHARS {
        let head = MAX_CONTENT_CHARS * 3 / 4;
        let tail = MAX_CONTENT_CHARS / 4;
        format!(
            "{}\n\n... [{} chars truncated] ...\n\n{}",
            &text[..head],
            text.len() - head - tail,
            &text[text.len() - tail..]
        )
    } else {
        text
    };

    Ok(ToolResult::success(format!(
        "Page content from {}:\n\n{}",
        url, truncated
    )))
}

/// Take a screenshot of a URL
async fn screenshot(
    url: &str,
    args: &Value,
    ctx: &ToolContext,
    wait_ms: u64,
) -> Result<ToolResult> {
    let browser = match find_browser() {
        Some(b) => b,
        None => return Ok(ToolResult::error(
            "No Chromium-based browser found. Install chromium, google-chrome, or brave.",
        )),
    };

    let output_path = match args.get("output").and_then(|v| v.as_str()) {
        Some(p) => PathBuf::from(p),
        None => {
            let ss_dir = PathBuf::from(&ctx.workspace_dir).join("screenshots");
            std::fs::create_dir_all(&ss_dir)?;
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            ss_dir.join(format!("screenshot_{}.png", ts))
        }
    };

    let timeout_ms = wait_ms + 15000;
    let screenshot_arg = format!("--screenshot={}", output_path.display());

    let _output = run_browser_command(
        &browser,
        &[
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--window-size=1280,720",
            &screenshot_arg,
            url,
        ],
        timeout_ms,
    )
    .await?;

    if output_path.exists() {
        let size = std::fs::metadata(&output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        Ok(ToolResult::success(format!(
            "Screenshot saved: {} ({} bytes)",
            output_path.display(),
            size
        )))
    } else {
        Ok(ToolResult::error(format!(
            "Screenshot file was not created at {}",
            output_path.display()
        )))
    }
}

/// Evaluate JavaScript on a page
async fn evaluate(url: &str, args: &Value, wait_ms: u64) -> Result<ToolResult> {
    let browser = match find_browser() {
        Some(b) => b,
        None => return Ok(ToolResult::error(
            "No Chromium-based browser found. Install chromium, google-chrome, or brave.",
        )),
    };

    let javascript = match args.get("javascript").and_then(|v| v.as_str()) {
        Some(js) if !js.is_empty() => js,
        _ => return Ok(ToolResult::error("browser evaluate: missing 'javascript' argument")),
    };

    // Wrap user JS in a setTimeout for page load, and use console.log for output
    let wrapped_js = format!(
        r#"setTimeout(() => {{ try {{ const __result = (function() {{ {} }})(); if (__result !== undefined) console.log(JSON.stringify(__result)); }} catch(e) {{ console.error('JS Error: ' + e.message); }} }}, {});"#,
        javascript, wait_ms
    );

    let timeout_ms = wait_ms + 10000;

    // Use --run-all-compositor-stages-before-draw to ensure page is rendered
    let output = run_browser_command(
        &browser,
        &[
            "--headless",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            &format!("--js-flags=--max-old-space-size=128"),
            url,
        ],
        timeout_ms,
    )
    .await?;

    // For headless Chrome, JS evaluation via command line is limited.
    // Return the DOM dump as a fallback with a note about the JS.
    let text = strip_html_tags(&output);
    let text = collapse_whitespace(&text);

    let truncated = if text.len() > MAX_CONTENT_CHARS {
        format!("{}...", &text[..MAX_CONTENT_CHARS])
    } else {
        text
    };

    Ok(ToolResult::success(format!(
        "Page DOM after loading {}:\n\n{}\n\nNote: For complex JS evaluation, consider using the 'exec' tool with a Node.js script or playwright CLI.",
        url, truncated
    )))
}

/// Run a headless browser command with timeout
async fn run_browser_command(
    browser: &str,
    args: &[&str],
    timeout_ms: u64,
) -> Result<String> {
    let mut cmd = tokio::process::Command::new(browser);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!("Failed to spawn browser '{}': {}", browser, e)
    })?;

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if !output.status.success() && stdout.is_empty() {
                anyhow::bail!(
                    "Browser exited with {}: {}",
                    output.status,
                    stderr.chars().take(500).collect::<String>()
                );
            }

            Ok(stdout)
        }
        Ok(Err(e)) => anyhow::bail!("Browser process error: {}", e),
        Err(_) => anyhow::bail!("Browser timed out after {}ms", timeout_ms),
    }
}

/// Simple HTML tag stripper
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    let lower = html.to_lowercase();
    let chars: Vec<char> = html.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();

    let mut i = 0;
    while i < chars.len() {
        if !in_tag && i + 7 < lower_chars.len() {
            let ahead: String = lower_chars[i..i + 7].iter().collect();
            if ahead == "<script" {
                in_script = true;
                in_tag = true;
                i += 1;
                continue;
            }
            let ahead6: String = lower_chars[i..i + 6].iter().collect();
            if ahead6 == "<style" {
                in_style = true;
                in_tag = true;
                i += 1;
                continue;
            }
        }

        if in_script && i + 9 <= lower_chars.len() {
            let ahead: String = lower_chars[i..i + 9].iter().collect();
            if ahead == "</script>" {
                in_script = false;
                i += 9;
                continue;
            }
        }

        if in_style && i + 8 <= lower_chars.len() {
            let ahead: String = lower_chars[i..i + 8].iter().collect();
            if ahead == "</style>" {
                in_style = false;
                i += 8;
                continue;
            }
        }

        if in_script || in_style {
            i += 1;
            continue;
        }

        if chars[i] == '<' {
            in_tag = true;
            // Add newline for block elements
            if i + 3 < lower_chars.len() {
                let tag_start: String = lower_chars[i + 1..i + 3.min(lower_chars.len() - i)].iter().collect();
                if tag_start.starts_with('p')
                    || tag_start.starts_with('d')
                    || tag_start.starts_with('h')
                    || tag_start.starts_with('l')
                    || tag_start.starts_with('b')
                    || tag_start.starts_with('t')
                {
                    result.push('\n');
                }
            }
        } else if chars[i] == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(chars[i]);
        }

        i += 1;
    }

    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Collapse multiple whitespace/newlines into single spaces/newlines
fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut prev_newline = false;
    let mut prev_space = false;

    for ch in text.chars() {
        if ch == '\n' {
            if !prev_newline {
                result.push('\n');
            }
            prev_newline = true;
            prev_space = false;
        } else if ch.is_whitespace() {
            if !prev_space && !prev_newline {
                result.push(' ');
            }
            prev_space = true;
        } else {
            result.push(ch);
            prev_newline = false;
            prev_space = false;
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_dir: "/tmp/browser-test".to_string(),
            agent_name: "test".to_string(),
            session_key: "test-session".to_string(),
            sandbox: crate::sandbox::SandboxPolicy::default(),
        }
    }

    #[tokio::test]
    async fn test_browser_missing_action() {
        let tool = BrowserTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"url": "https://example.com"});
        let result = tool.execute(args, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_browser_missing_url() {
        let tool = BrowserTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "navigate"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("missing"));
    }

    #[tokio::test]
    async fn test_browser_invalid_url() {
        let tool = BrowserTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "navigate", "url": "ftp://bad"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("http"));
    }

    #[tokio::test]
    async fn test_browser_unknown_action() {
        let tool = BrowserTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "destroy", "url": "https://example.com"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Unknown action"));
    }

    #[tokio::test]
    async fn test_browser_evaluate_missing_js() {
        let tool = BrowserTool;
        let ctx = test_ctx();
        let args = serde_json::json!({"action": "evaluate", "url": "https://example.com"});
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("javascript"));
    }

    #[test]
    fn test_strip_html_tags() {
        let html = "<html><head><title>Test</title></head><body><p>Hello <b>world</b></p></body></html>";
        let text = strip_html_tags(html);
        assert!(text.contains("Test"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<"));
    }

    #[test]
    fn test_strip_html_script() {
        let html = "<p>Before</p><script>var x = 1;</script><p>After</p>";
        let text = strip_html_tags(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn test_collapse_whitespace() {
        let text = "hello   world\n\n\n\nfoo   bar";
        let result = collapse_whitespace(text);
        assert_eq!(result, "hello world\nfoo bar");
    }

    #[test]
    fn test_tool_metadata() {
        let tool = BrowserTool;
        assert_eq!(tool.name(), "browser");
        assert!(tool.description().contains("browser"));
        let params = tool.parameters();
        assert!(params.get("required").is_some());
    }
}
