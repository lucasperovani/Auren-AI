//! Common LLM provider interface (spec section 11).
//!
//! Providers implement this trait; the agent runtime depends only on the
//! interface, never on provider-specific logic. Model capabilities are
//! explicit: not every model supports tool calling or structured output.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String, // system | user | assistant | tool
    pub content: String,
    /// Tool call requests attached to an assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<LlmToolCall>>,
    /// Tool result for a `tool` role message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A tool definition exposed to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Model capabilities, discovered or configured (spec section 2.2: never
/// assume a model supports tool calling or structured output).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub max_context_tokens: Option<u64>,
}

/// Non-streaming completion response.
#[derive(Debug, Clone)]
pub struct LlmCompletion {
    pub content: String,
    pub tool_calls: Vec<LlmToolCall>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub finish_reason: Option<String>,
}

/// Errors surfaced by providers. Provider internals are never leaked to
/// clients; the runtime logs details and returns a safe message.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("model does not support the requested capability")]
    UnsupportedCapability,
    #[error("provider request timed out")]
    Timeout,
    #[error("provider rate limit exceeded")]
    RateLimited,
    #[error("provider authentication failed")]
    AuthenticationFailed,
    #[error("provider request failed")]
    RequestFailed,
}

/// Common provider interface (spec section 11).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Provider identifier for logging and configuration.
    fn name(&self) -> &str;

    /// Capabilities of the configured model.
    fn capabilities(&self) -> ModelCapabilities;

    /// Send a conversation and receive a completion. Tool definitions may
    /// be empty; providers without tool-calling support reject them.
    async fn complete(
        &self,
        messages: &[LlmMessage],
        tools: &[LlmToolDefinition],
        max_tokens: Option<u64>,
    ) -> Result<LlmCompletion, LlmError>;
}

/// Build a provider client from a stored configuration row.
pub fn build_provider(
    provider_type: &str,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    match provider_type {
        "openai_compatible" | "openai" => Ok(Box::new(providers::OpenAiCompatibleProvider::new(
            base_url, api_key, model,
        ))),
        "ollama" => Ok(Box::new(providers::OpenAiCompatibleProvider::new(
            &format!("{base_url}/v1"),
            None,
            model,
        ))),
        "anthropic" | "gemini" => {
            // Gemini and Anthropic use different wire formats; both are
            // reachable through their OpenAI-compatible endpoints where
            // available, otherwise they are not yet supported.
            Err(LlmError::UnsupportedCapability)
        }
        other => {
            tracing::warn!(provider_type = other, "unknown provider type");
            Err(LlmError::UnsupportedCapability)
        }
    }
}

pub mod providers {
    //! Concrete provider implementations.

    use super::*;

    /// OpenAI-compatible chat-completions client. Covers OpenAI,
    /// OpenAI-compatible APIs, and Ollama's `/v1` endpoint.
    pub struct OpenAiCompatibleProvider {
        base_url: String,
        api_key: Option<String>,
        model: String,
        client: reqwest::Client,
    }

    impl OpenAiCompatibleProvider {
        pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
            Self {
                base_url: base_url.trim_end_matches('/').to_string(),
                api_key: api_key.map(|k| k.to_string()),
                model: model.to_string(),
                client: reqwest::Client::new(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for OpenAiCompatibleProvider {
        fn name(&self) -> &str {
            "openai_compatible"
        }

        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                tool_calling: true,
                structured_output: true,
                streaming: false,
                max_context_tokens: None,
            }
        }

        async fn complete(
            &self,
            messages: &[LlmMessage],
            tools: &[LlmToolDefinition],
            max_tokens: Option<u64>,
        ) -> Result<LlmCompletion, LlmError> {
            let mut body = serde_json::json!({
                "model": self.model,
                "messages": messages,
            });
            if let Some(max) = max_tokens {
                body["max_tokens"] = serde_json::json!(max);
            }
            if !tools.is_empty() {
                body["tools"] = serde_json::json!(tools
                    .iter()
                    .map(|t| serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    }))
                    .collect::<Vec<_>>());
            }

            let mut request = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .timeout(std::time::Duration::from_secs(120))
                .json(&body);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key);
            }
            let response = request.send().await.map_err(|e| {
                if e.is_timeout() {
                    LlmError::Timeout
                } else {
                    tracing::debug!(error = %e, "llm request failed");
                    LlmError::RequestFailed
                }
            })?;

            match response.status() {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    return Err(LlmError::AuthenticationFailed)
                }
                reqwest::StatusCode::TOO_MANY_REQUESTS => return Err(LlmError::RateLimited),
                status if !status.is_success() => {
                    tracing::debug!(status = status.as_u16(), "llm provider error");
                    return Err(LlmError::RequestFailed);
                }
                _ => {}
            }

            let body_bytes = response.bytes().await.map_err(|_| LlmError::RequestFailed)?;
            let payload: Value = serde_json::from_slice(&body_bytes).map_err(|_| LlmError::RequestFailed)?;
            let choice = payload
                .get("choices")
                .and_then(|c| c.get(0))
                .ok_or(LlmError::RequestFailed)?;
            let message = choice.get("message").cloned().unwrap_or(serde_json::json!({}));
            let tool_calls = message
                .get("tool_calls")
                .and_then(|tc| tc.as_array())
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|c| {
                            Some(LlmToolCall {
                                id: c.get("id")?.as_str()?.to_string(),
                                name: c.get("function")?.get("name")?.as_str()?.to_string(),
                                arguments: serde_json::from_str(
                                    c.get("function")?.get("arguments")?.as_str().unwrap_or("{}"),
                                )
                                .unwrap_or(serde_json::json!({})),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let usage = payload.get("usage").cloned().unwrap_or(serde_json::json!({}));

            Ok(LlmCompletion {
                content: message.get("content").and_then(|c| c.as_str()).unwrap_or_default().to_string(),
                tool_calls,
                input_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()),
                output_tokens: usage.get("completion_tokens").and_then(|v| v.as_u64()),
                finish_reason: choice.get("finish_reason").and_then(|v| v.as_str()).map(|s| s.to_string()),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn build_provider_maps_types() {
            assert!(build_provider("openai_compatible", "https://api.example.com", Some("k"), "m").is_ok());
            assert!(build_provider("ollama", "http://127.0.0.1:11434", None, "m").is_ok());
            assert!(build_provider("unknown", "https://x", None, "m").is_err());
        }

        #[tokio::test]
        async fn complete_parses_tool_calls() {
            // Mock server via a local hyper-less approach: use a tiny TCP
            // server that answers a canned OpenAI response.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            // Signal readiness before spawning the provider request.
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = ready_tx.send(());
                let (stream, _) = listener.accept().await.unwrap();
                let http = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                );
                let service = hyper::service::service_fn(|_req| async {
                    let body = serde_json::json!({
                        "choices": [{
                            "message": {
                                "content": "hi",
                                "tool_calls": [{
                                    "id": "c1",
                                    "type": "function",
                                    "function": { "name": "search", "arguments": "{\"q\":\"x\"}" }
                                }],
                                "finish_reason": "tool_calls"
                            }
                        }],
                        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
                    });
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        axum::body::Body::from(body.to_string()),
                    ))
                });
                let _ = http.serve_connection_with_upgrades(
                    hyper_util::rt::TokioIo::new(stream),
                    service,
                ).await;
            });

            ready_rx.await.unwrap();
            let provider = OpenAiCompatibleProvider::new(
                &format!("http://{addr}"),
                Some("key"),
                "test-model",
            );
            let completion = provider
                .complete(&[LlmMessage { role: "user".into(), content: "hello".into(), tool_calls: None, tool_call_id: None }], &[], None)
                .await
                .unwrap();
            assert_eq!(completion.content, "hi");
            assert_eq!(completion.tool_calls.len(), 1);
            assert_eq!(completion.tool_calls[0].name, "search");
            assert_eq!(completion.tool_calls[0].arguments["q"], "x");
            assert_eq!(completion.input_tokens, Some(10));
        }
    }
}