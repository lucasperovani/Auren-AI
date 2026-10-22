//! MCP (Model Context Protocol) client support.
//!
//! Connects to external MCP servers over HTTP (Streamable HTTP transport),
//! lists their tools, and exposes them through the tool registry so the
//! agent can invoke them like any other tool. Tool invocations pass through
//! the same permission engine as built-in tools.

use serde_json::{json, Value};
use std::time::Duration;

use crate::error::ApiError;

/// An MCP server connection configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpServerConfig {
    /// Unique server identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Server URL (Streamable HTTP endpoint), for example
    /// `https://mcp.example.com/mcp`.
    pub url: String,
    /// Optional bearer token or header-based auth.
    pub auth_header: Option<String>,
    /// Whether the connection is enabled.
    pub enabled: bool,
}

/// A tool exposed by an MCP server.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpTool {
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// JSON-RPC request id counter (per process; MCP requires unique ids per
/// connection, and each call here opens a fresh HTTP session).
fn next_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Send a JSON-RPC request to an MCP server and return the result.
async fn rpc(
    server: &McpServerConfig,
    method: &str,
    params: Value,
) -> Result<Value, ApiError> {
    let id = next_request_id();
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let mut request = reqwest::Client::new()
        .post(&server.url)
        .timeout(Duration::from_secs(60))
        .header("Accept", "application/json, text/event-stream")
        .json(&body);
    if let Some(auth) = &server.auth_header {
        request = request.header("Authorization", auth);
    }
    let response = request.send().await.map_err(|e| {
        tracing::debug!(error = %e, server = %server.name, "mcp request failed");
        ApiError::ServiceUnavailable
    })?;
    if !response.status().is_success() {
        return Err(ApiError::Conflict(format!(
            "MCP server '{}' returned HTTP {}",
            server.name,
            response.status().as_u16()
        )));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();

    // Streamable HTTP may answer with JSON or an SSE stream containing one
    // data event; handle both.
    let text = response.text().await.map_err(|_| ApiError::ServiceUnavailable)?;
    let payload: Value = if content_type.contains("text/event-stream") {
        parse_sse_payload(&text).ok_or_else(|| {
            ApiError::Conflict("MCP server returned an SSE stream without a JSON-RPC response".into())
        })?
    } else {
        serde_json::from_str(&text)
            .map_err(|_| ApiError::Conflict("MCP server returned a non-JSON response".into()))?
    };

    if let Some(error) = payload.get("error") {
        return Err(ApiError::Conflict(format!(
            "MCP error: {}",
            error.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")
        )));
    }
    Ok(payload.get("result").cloned().unwrap_or(json!({})))
}

/// Extract the first JSON-RPC response from an SSE body.
fn parse_sse_payload(text: &str) -> Option<Value> {
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                if value.get("result").is_some() || value.get("error").is_some() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// List the tools an MCP server exposes.
pub async fn list_server_tools(server: &McpServerConfig) -> Result<Vec<McpTool>, ApiError> {
    let result = rpc(server, "tools/list", json!({})).await?;
    let tools = result
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(tools
        .iter()
        .filter_map(|t| {
            Some(McpTool {
                server_id: server.id.clone(),
                name: t.get("name")?.as_str()?.to_string(),
                description: t.get("description").and_then(|d| d.as_str()).unwrap_or_default().to_string(),
                input_schema: t.get("inputSchema").cloned().unwrap_or(json!({ "type": "object" })),
            })
        })
        .collect())
}

/// Invoke an MCP tool.
pub async fn invoke_server_tool(
    server: &McpServerConfig,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value, ApiError> {
    let result = rpc(
        server,
        "tools/call",
        json!({ "name": tool_name, "arguments": arguments }),
    )
    .await?;
    // MCP tool results carry content blocks; concatenate text blocks.
    let content = result.get("content").and_then(|c| c.as_array());
    let mut text_parts = Vec::new();
    if let Some(blocks) = content {
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
        }
    }
    if text_parts.is_empty() {
        Ok(result)
    } else {
        Ok(json!({ "text": text_parts.join("\n") }))
    }
}

/// Initialize handshake (capability exchange). Some servers require it
/// before listing tools.
pub async fn initialize(server: &McpServerConfig) -> Result<Value, ApiError> {
    rpc(
        server,
        "initialize",
        json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "auren-ai", "version": "0.1.0" }
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock MCP server speaking Streamable HTTP JSON-RPC.
    async fn start_mock_mcp() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break };
                let service = hyper::service::service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                    let (_, body) = req.into_parts();
                    let bytes = axum::body::to_bytes(axum::body::Body::new(body), usize::MAX).await.unwrap();
                    let request: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
                    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let result = match method {
                        "initialize" => json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "mock-mcp", "version": "1.0.0" }
                        }),
                        "tools/list" => json!({
                            "tools": [
                                { "name": "echo", "description": "Echo the input", "inputSchema": { "type": "object" } },
                                { "name": "add", "description": "Add two numbers", "inputSchema": { "type": "object" } }
                            ]
                        }),
                        "tools/call" => {
                            let name = request["params"]["name"].as_str().unwrap_or("");
                            if name == "add" {
                                let a = request["params"]["arguments"]["a"].as_f64().unwrap_or(0.0);
                                let b = request["params"]["arguments"]["b"].as_f64().unwrap_or(0.0);
                                json!({ "content": [{ "type": "text", "text": format!("{}", a + b) }] })
                            } else {
                                json!({ "content": [{ "type": "text", "text": "echoed" }] })
                            }
                        }
                        _ => json!({}),
                    };
                    let body = json!({ "jsonrpc": "2.0", "id": request.get("id").cloned().unwrap_or(json!(0)), "result": result });
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(axum::body::Body::from(
                        body.to_string(),
                    )))
                });
                let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service)
                    .await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn lists_and_invokes_mcp_tools() {
        let url = start_mock_mcp().await;
        let server = McpServerConfig {
            id: "mock".into(),
            name: "Mock MCP".into(),
            url,
            auth_header: None,
            enabled: true,
        };

        let tools = list_server_tools(&server).await.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "echo");

        let result = invoke_server_tool(&server, "add", &json!({ "a": 2, "b": 3 })).await.unwrap();
        assert_eq!(result["text"], "5");
    }

    #[tokio::test]
    async fn sse_responses_are_parsed() {
        let payload = parse_sse_payload("event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n");
        assert!(payload.is_some());
        assert_eq!(payload.unwrap()["result"]["ok"], true);
    }
}