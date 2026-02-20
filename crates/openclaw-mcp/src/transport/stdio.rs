use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::info;

use crate::protocol::JsonRpcRequest;
use crate::server::McpServer;

/// Run the MCP server over stdin/stdout (newline-delimited JSON-RPC)
pub async fn run_stdio(server: McpServer) -> anyhow::Result<()> {
    info!("MCP stdio transport starting");

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err_response = crate::protocol::JsonRpcResponse::error(
                    None,
                    crate::protocol::PARSE_ERROR,
                    format!("Parse error: {}", e),
                );
                let out = serde_json::to_string(&err_response).unwrap();
                stdout.write_all(out.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };

        // Notifications (no id) don't get responses
        if request.id.is_none() {
            // Handle "notifications/initialized" etc.
            info!("MCP notification: {}", request.method);
            continue;
        }

        let response = server.handle_request(&request).await;
        let out = serde_json::to_string(&response).unwrap();
        stdout.write_all(out.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;

        // If shutdown was requested, exit
        if request.method == "shutdown" {
            info!("MCP shutdown requested, exiting");
            break;
        }
    }

    info!("MCP stdio transport stopped");
    Ok(())
}
