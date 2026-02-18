use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};

const MAX_RESULTS: usize = 8;
const TIMEOUT_SECS: u64 = 15;

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo and return a list of results with titles, URLs, and snippets."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 8)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("web_search: missing 'query' argument"))?;

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(MAX_RESULTS);

        match search_ddg(query, max_results).await {
            Ok(results) => {
                if results.is_empty() {
                    Ok(ToolResult::success("No results found."))
                } else {
                    let mut output = format!("Search results for: {}\n\n", query);
                    for (i, r) in results.iter().enumerate() {
                        output.push_str(&format!(
                            "{}. {}\n   {}\n   {}\n\n",
                            i + 1,
                            r.title,
                            r.url,
                            r.snippet
                        ));
                    }
                    Ok(ToolResult::success(output))
                }
            }
            Err(e) => Ok(ToolResult::error(format!("Search failed: {}", e))),
        }
    }
}

struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

async fn search_ddg(query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()?;

    // DuckDuckGo HTML search
    let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding::encode(query));

    let response = client
        .get(&url)
        .send()
        .await?;

    let body = response.text().await?;

    // Parse results from HTML
    let results = parse_ddg_html(&body, max_results);

    Ok(results)
}

fn parse_ddg_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // DuckDuckGo HTML results are in <div class="result"> blocks
    // Each has: <a class="result__a" href="...">title</a>
    //           <a class="result__snippet">snippet</a>
    for result_block in html.split("class=\"result__body\"") {
        if results.len() >= max_results {
            break;
        }

        // Extract URL from result__a href
        let url = extract_between(result_block, "class=\"result__a\" href=\"", "\"")
            .map(|u| decode_ddg_url(u))
            .unwrap_or_default();

        if url.is_empty() {
            continue;
        }

        // Extract title from result__a content
        let title = extract_between(result_block, "class=\"result__a\"", "</a>")
            .map(|t| {
                // Strip the opening > from the tag
                let t = t.trim_start_matches('>');
                strip_html_tags(t).trim().to_string()
            })
            .unwrap_or_default();

        // Extract snippet
        let snippet = extract_between(result_block, "class=\"result__snippet\"", "</a>")
            .or_else(|| extract_between(result_block, "class=\"result__snippet\"", "</td>"))
            .map(|s| {
                let s = s.trim_start_matches('>');
                strip_html_tags(s).trim().to_string()
            })
            .unwrap_or_default();

        if !title.is_empty() || !snippet.is_empty() {
            results.push(SearchResult {
                title: if title.is_empty() { url.clone() } else { title },
                url,
                snippet: if snippet.is_empty() {
                    "(no snippet)".to_string()
                } else {
                    snippet
                },
            });
        }
    }

    results
}

fn decode_ddg_url(url: &str) -> String {
    // DuckDuckGo wraps URLs in redirect: //duckduckgo.com/l/?uddg=<encoded_url>&...
    if let Some(uddg_start) = url.find("uddg=") {
        let encoded = &url[uddg_start + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        urlencoding::decode(encoded)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| url.to_string())
    } else if url.starts_with("http") {
        url.to_string()
    } else if url.starts_with("//") {
        format!("https:{}", url)
    } else {
        url.to_string()
    }
}

fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let remaining = &text[start_idx..];
    let end_idx = remaining.find(end)?;
    Some(&remaining[..end_idx])
}

fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"x\">link</a>"), "link");
        assert_eq!(strip_html_tags("&amp; &lt;"), "& <");
    }

    #[test]
    fn test_decode_ddg_url() {
        let url = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=abc";
        assert_eq!(decode_ddg_url(url), "https://example.com");
    }

    #[test]
    fn test_extract_between() {
        let text = r#"class="result__a" href="https://example.com">Title</a>"#;
        let url = extract_between(text, "href=\"", "\"").unwrap();
        assert_eq!(url, "https://example.com");
    }
}
