use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};

const MAX_BODY_BYTES: usize = 128 * 1024;
const TIMEOUT_SECS: u64 = 20;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as text. HTML pages are converted to readable text. Useful for reading web pages, APIs, or downloading text content."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "raw": {
                    "type": "boolean",
                    "description": "If true, return raw response body without HTML-to-text conversion (default: false)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("web_fetch: missing 'url' argument"))?;

        let raw = args
            .get("raw")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validate URL
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolResult::error("URL must start with http:// or https://"));
        }

        match fetch_url(url, raw).await {
            Ok(content) => Ok(ToolResult::success(content)),
            Err(e) => Ok(ToolResult::error(format!("Fetch failed: {}", e))),
        }
    }
}

async fn fetch_url(url: &str, raw: bool) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()?;

    let response = client.get(url).send().await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !status.is_success() {
        anyhow::bail!("HTTP {}: {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
    }

    let bytes = response.bytes().await?;
    if bytes.len() > MAX_BODY_BYTES {
        let truncated = String::from_utf8_lossy(&bytes[..MAX_BODY_BYTES]);
        return Ok(format!("{}\n\n... (truncated at {}KB)", truncated, MAX_BODY_BYTES / 1024));
    }

    let body = String::from_utf8_lossy(&bytes).to_string();

    if raw {
        return Ok(body);
    }

    // If HTML, convert to readable text
    if content_type.contains("text/html") || body.trim_start().starts_with("<!") || body.trim_start().starts_with("<html") {
        Ok(html_to_text(&body))
    } else {
        Ok(body)
    }
}

/// Convert HTML to readable plain text
fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut tag_name = String::new();
    let mut collecting_tag = false;
    let mut last_was_space = false;

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                collecting_tag = true;
                tag_name.clear();
            }
            '>' => {
                in_tag = false;
                collecting_tag = false;

                let tag_lower = tag_name.to_lowercase();
                let tag_base = tag_lower.split_whitespace().next().unwrap_or("");

                match tag_base {
                    "script" => in_script = true,
                    "/script" => in_script = false,
                    "style" => in_style = true,
                    "/style" => in_style = false,
                    "br" | "br/" => {
                        text.push('\n');
                        last_was_space = true;
                    }
                    "p" | "/p" | "div" | "/div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "/h1" | "/h2" | "/h3" | "/h4" | "/h5" | "/h6" | "li" | "tr" | "/tr"
                    | "blockquote" | "/blockquote" | "hr" | "hr/" => {
                        if !text.ends_with('\n') {
                            text.push('\n');
                        }
                        last_was_space = true;
                    }
                    "td" | "th" => {
                        text.push('\t');
                        last_was_space = true;
                    }
                    _ => {}
                }
            }
            _ if in_tag => {
                if collecting_tag {
                    tag_name.push(ch);
                }
            }
            _ if in_script || in_style => {}
            _ => {
                if ch.is_whitespace() {
                    if !last_was_space {
                        text.push(' ');
                        last_was_space = true;
                    }
                } else {
                    text.push(ch);
                    last_was_space = false;
                }
            }
        }
    }

    // Decode HTML entities
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse multiple blank lines
    let mut result = String::new();
    let mut blank_count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(trimmed);
            result.push('\n');
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_text_basic() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b>.</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello world."));
    }

    #[test]
    fn test_html_to_text_strips_scripts() {
        let html = "<p>Before</p><script>var x = 1;</script><p>After</p>";
        let text = html_to_text(html);
        assert!(text.contains("Before"));
        assert!(text.contains("After"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn test_html_to_text_entities() {
        let html = "<p>A &amp; B &lt; C</p>";
        let text = html_to_text(html);
        assert!(text.contains("A & B < C"));
    }

    #[test]
    fn test_url_validation() {
        // This tests the validation logic, not actual fetching
        let tool = WebFetchTool;
        let args = serde_json::json!({"url": "ftp://bad.com"});
        let ctx = super::super::ToolContext {
            workspace_dir: "/tmp".to_string(),
            agent_name: "test".to_string(),
            session_key: "test".to_string(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(args, &ctx)).unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("http"));
    }
}
