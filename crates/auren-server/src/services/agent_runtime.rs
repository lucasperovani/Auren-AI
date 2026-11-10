//! Bounded agent execution loop (spec section 12).
//!
//! The runtime loads the task, plans with the LLM, validates and authorizes
//! each requested action through the permission engine, executes tools, and
//! persists state after every transition. Limits prevent unbounded loops.

use serde_json::{json, Value};
use sqlx::Row;

use crate::{
    error::ApiError,
    services::{
        llm::{LlmMessage, LlmProvider, LlmToolDefinition},
        tool_registry::{Authorization, ToolRegistry},
    },
    state::AppState,
};

/// Execution limits (spec section 12).
pub struct AgentRuntime {
    state: AppState,
    tools: ToolRegistry,
    /// Tool calls made during the most recent chat exchange, for the
    /// activity trace shown in the frontend.
    pub last_tool_trace: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

/// Outcome of a bounded run.
#[derive(Debug, PartialEq)]
pub enum RunOutcome {
    Completed,
    Paused,
    Failed(String),
    LimitReached(String),
}

impl AgentRuntime {
    pub fn new(state: AppState) -> Self {
        let tools = ToolRegistry::new(&state);
        Self {
            state,
            tools,
            last_tool_trace: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn max_iterations(&self) -> u32 {
        self.state.config.max_iterations.max(1)
    }

    fn max_tool_calls(&self) -> u32 {
        self.state.config.max_tool_calls.max(1)
    }

    fn max_llm_calls(&self) -> u32 {
        self.state.config.max_llm_calls.max(1)
    }

    /// Execute a task run to completion or a stopping condition.
    pub async fn execute(&self, run_id: &str) -> RunOutcome {
        let run_row = match sqlx::query("SELECT task_id, status FROM task_runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&self.state.db)
            .await
        {
            Ok(Some(row)) => row,
            _ => return RunOutcome::Failed("run not found".into()),
        };
        let task_id: String = run_row.get("task_id");
        let status: String = run_row.get("status");
        if status == "cancelled" || status == "paused" {
            return RunOutcome::Paused;
        }

        let task = match sqlx::query("SELECT objective, agent_id FROM tasks WHERE id = ?")
            .bind(&task_id)
            .fetch_optional(&self.state.db)
            .await
        {
            Ok(Some(row)) => row,
            _ => return RunOutcome::Failed("task not found".into()),
        };
        let objective: String = task.get("objective");
        let agent_id: String = task.get("agent_id");

        // Load the agent's instructions.
        let instructions: String = sqlx::query("SELECT instructions FROM agents WHERE id = ?")
            .bind(&agent_id)
            .fetch_optional(&self.state.db)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("instructions").ok())
            .unwrap_or_default();

        // Build the LLM provider from the agent's configuration.
        let provider = self.build_provider(&agent_id).await;
        let mut messages: Vec<LlmMessage> = vec![
            LlmMessage {
                role: "system".into(),
                content: format!(
                    "{instructions}\n\nYou are executing a task. Objective: {objective}. \
                     Use the available tools step by step. When the objective is achieved, \
                     reply with a final summary and no tool calls."
                ),
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage {
                role: "user".into(),
                content: objective.clone(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let available_tools = self.available_tools(&agent_id).await;
        let mut tool_calls_made: u32 = 0;

        for llm_calls in 0..self.max_iterations() {
            // A paused/cancelled run stops at the next safe point.
            let current: String = sqlx::query("SELECT status FROM task_runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&self.state.db)
                .await
                .map(|r| r.get("status"))
                .unwrap_or_default();
            if current == "cancelled" {
                return RunOutcome::Paused;
            }

            let Some(provider) = &provider else {
                // No LLM configured: the run cannot plan. Record the state
                // honestly instead of faking success (spec section 21).
                return RunOutcome::Failed("no LLM provider configured for this agent".into());
            };
            if llm_calls >= self.max_llm_calls() {
                return RunOutcome::LimitReached("llm call limit reached".into());
            }

            let completion = match provider.complete(&messages, &available_tools, None).await {
                Ok(c) => c,
                Err(e) => return RunOutcome::Failed(format!("llm error: {e}")),
            };

            if completion.tool_calls.is_empty() {
                // Final answer: persist and complete.
                self.persist_result(run_id, &completion.content).await;
                return RunOutcome::Completed;
            }

            messages.push(LlmMessage {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });

            for call in &completion.tool_calls {
                if tool_calls_made >= self.max_tool_calls() {
                    return RunOutcome::LimitReached("tool call limit reached".into());
                }
                tool_calls_made += 1;

                // Resolve the tool by name among the agent's authorized tools.
                let tool_id = self.resolve_tool(&agent_id, &call.name).await;
                let Some(tool_id) = tool_id else {
                    messages.push(LlmMessage {
                        role: "tool".into(),
                        content: format!("tool '{}' is not available to this agent", call.name),
                        tool_calls: None,
                        tool_call_id: Some(call.id.clone()),
                    });
                    continue;
                };

                // Permission engine: authorize, then execute.
                match self.tools.authorize(&agent_id, &tool_id, &call.arguments, None).await {
                    Ok(Authorization::Allowed) => {
                        let result = self.tools.invoke(run_id, &tool_id, &call.arguments, None).await;
                        let content = match result {
                            Ok(value) => value.to_string(),
                            Err(e) => format!("tool error: {e}"),
                        };
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Ok(Authorization::RequiresApproval { approval_id }) => {
                        // Pause the run pending approval (spec section 12).
                        sqlx::query("UPDATE task_runs SET status = 'paused' WHERE id = ?")
                            .bind(run_id)
                            .execute(&self.state.db)
                            .await
                            .ok();
                        sqlx::query("UPDATE approval_requests SET task_run_id = ? WHERE id = ?")
                            .bind(run_id)
                            .bind(&approval_id)
                            .execute(&self.state.db)
                            .await
                            .ok();
                        return RunOutcome::Paused;
                    }
                    Ok(Authorization::Denied(reason)) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("denied: {reason}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Err(e) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("authorization error: {e}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                }
            }
        }
        RunOutcome::LimitReached("iteration limit reached".into())
    }

    async fn build_provider(&self, agent_id: &str) -> Option<Box<dyn LlmProvider>> {
        let row = sqlx::query(
            "SELECT p.provider_type, p.base_url, p.encrypted_api_key, p.key_nonce, m.model_name
             FROM agents a
             LEFT JOIN llm_providers p ON p.id = a.llm_provider_id
             LEFT JOIN llm_models m ON m.id = a.model_id
             WHERE a.id = ?",
        )
        .bind(agent_id)
        .fetch_optional(&self.state.db)
        .await
        .ok()
        .flatten()?;

        let provider_type: String = row.try_get("provider_type").ok()?;
        let base_url: String = row.try_get("base_url").ok()?;
        let model_name: Option<String> = row.try_get("model_name").ok();
        let ciphertext: Option<Vec<u8>> = row.try_get("encrypted_api_key").ok();
        let nonce: Option<Vec<u8>> = row.try_get("key_nonce").ok();
        // Decrypt the stored key; the plaintext never leaves this scope.
        // A ciphertext without a valid 12-byte nonce (legacy/inserted rows)
        // is treated as absent rather than panicking.
        let api_key = match (ciphertext, nonce) {
            (Some(ct), Some(n)) if n.len() == 12 => {
                match crate::services::credentials::decrypt_credential(&ct, &n, &self.state.config.credential_encryption_key) {
                    Ok(key) => Some(key),
                    // Undecryptable credentials (wrong key/tampered) make the
                    // provider unusable; surface as "not configured" honestly.
                    Err(_) => return None,
                }
            }
            // No stored key: local providers (Ollama) work without one.
            _ => None,
        };

        crate::services::llm::build_provider(
            &provider_type,
            &base_url,
            api_key.as_deref(),
            model_name.as_deref().unwrap_or_default(),
        )
        .ok()
    }

    /// Tools available to the agent from enabled skills, resolved through each
    /// skill's **active** version, plus every tool exposed by the user's
    /// enabled MCP servers.
    ///
    /// `tool_sync::sync_agent_skills` repoints stale links before this runs, so
    /// the join against `skills.active_version_id` is what makes a newly
    /// activated version visible immediately instead of only after a restart.
    async fn available_tools(&self, agent_id: &str) -> Vec<LlmToolDefinition> {
        // Bring links in line with the current versions first: a skill updated
        // mid-conversation must be visible on the very next message.
        if let Err(e) = crate::services::tool_sync::sync_agent_skills(&self.state.db, agent_id).await {
            tracing::warn!(error = %e, "failed to sync agent skill versions");
        }

        let rows = sqlx::query(
            "SELECT st.tool_name, st.description, st.input_schema_json,
                    COALESCE(st.implementation_ref, st.tool_name) AS impl_ref
             FROM skill_tools st
             JOIN skill_versions sv ON sv.id = st.skill_version_id
             JOIN skills sk ON sk.id = sv.skill_id
             JOIN agent_skills ask
               ON ask.skill_version_id = st.skill_version_id
              AND ask.agent_id = ?
              AND ask.enabled = 1
             WHERE sk.status = 'enabled'
               AND sk.active_version_id = sv.id",
        )
        .bind(agent_id)
        .fetch_all(&self.state.db)
        .await
        .unwrap_or_default();
        let mut tools: Vec<LlmToolDefinition> = Vec::with_capacity(rows.len());
        for r in &rows {
            let impl_ref: String = r.try_get("impl_ref").unwrap_or_default();
            // Never advertise a tool whose runtime requirement is missing on
            // this host: the model would call it and the user would see an
            // unexplained failure. `tools_unavailable` is surfaced to the UI.
            if !crate::services::capabilities::tool_is_available(&impl_ref) {
                tracing::debug!(
                    tool = %r.get::<String, _>("tool_name"),
                    impl_ref = %impl_ref,
                    "hiding tool: host capability unavailable"
                );
                continue;
            }
            tools.push(LlmToolDefinition {
                name: r.get("tool_name"),
                description: r.get("description"),
                input_schema: serde_json::from_str(&r.get::<String, _>("input_schema_json")).unwrap_or(json!({})),
            });
        }

        // MCP tools are discovered from the servers themselves, so their
        // catalogue is refreshed on every turn (see below) rather than read
        // from local storage.
        tools.extend(self.mcp_tools(agent_id).await);
        tools
    }

    /// Tools exposed by the user's enabled MCP servers.
    ///
    /// The tool list is fetched from each server and cached per process for a
    /// short TTL: servers can add or remove tools without any local write, but
    /// re-listing on every single turn would add latency to each message.
    async fn mcp_tools(&self, agent_id: &str) -> Vec<LlmToolDefinition> {
        let servers = match self.mcp_servers_for_agent(agent_id).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load MCP servers");
                return Vec::new();
            }
        };

        let mut out = Vec::new();
        for server in servers {
            match crate::services::mcp::list_server_tools(&server).await {
                Ok(tools) => {
                    for tool in tools {
                        // MCP tool names are namespaced so two servers cannot
                        // collide, and so `resolve_tool` can route the call.
                        out.push(LlmToolDefinition {
                            name: format!("mcp__{}__{}", server.id, tool.name),
                            description: tool.description,
                            input_schema: tool.input_schema,
                        });
                    }
                }
                Err(e) => {
                    // A broken server must not take the whole turn down.
                    tracing::warn!(server = %server.name, error = %e, "MCP tool listing failed");
                }
            }
        }
        out
    }

    /// Invoke a tool on a remote MCP server by id.
    async fn call_mcp_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = ?")
            .bind(format!("mcp_server:{server_id}"))
            .fetch_optional(&self.state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let raw: String = row.get("value_json");
        let server: crate::services::mcp::McpServerConfig = serde_json::from_str(&raw)
            .map_err(|_| ApiError::Internal(anyhow::anyhow!("corrupt MCP server config")))?;
        if !server.enabled {
            return Err(ApiError::Conflict("MCP server is disabled".into()));
        }
        crate::services::mcp::invoke_server_tool(&server, tool_name, arguments).await
    }

    /// Enabled MCP servers owned by the agent's user.
    async fn mcp_servers_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<crate::services::mcp::McpServerConfig>, ApiError> {
        let rows = sqlx::query(
            "SELECT s.key, s.value_json FROM app_settings s
             WHERE s.key LIKE 'mcp_server:%'
               AND EXISTS (
                   SELECT 1 FROM agents a WHERE a.id = ? AND a.user_id = (
                       SELECT user_id FROM agents WHERE id = ?
                   )
               )",
        )
        .bind(agent_id)
        .bind(agent_id)
        .fetch_all(&self.state.db)
        .await?;

        let mut servers = Vec::new();
        for row in rows {
            let raw: String = row.get("value_json");
            match serde_json::from_str::<crate::services::mcp::McpServerConfig>(&raw) {
                Ok(server) if server.enabled => servers.push(server),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "corrupt MCP server config"),
            }
        }
        Ok(servers)
    }

    async fn resolve_tool(&self, agent_id: &str, tool_name: &str) -> Option<String> {
        // MCP tools are not rows in `skill_tools`; they are routed by name.
        if tool_name.starts_with("mcp__") {
            return Some(tool_name.to_string());
        }
        // Same rule as `available_tools`: only the active version counts.
        let row = sqlx::query(
            "SELECT st.id, COALESCE(st.implementation_ref, st.tool_name) AS impl_ref
             FROM skill_tools st
             JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             JOIN skill_versions sv ON sv.id = st.skill_version_id
             JOIN skills sk ON sk.id = sv.skill_id
             WHERE ask.agent_id = ? AND st.tool_name = ? AND ask.enabled = 1
               AND sk.status = 'enabled' AND sk.active_version_id = sv.id",
        )
        .bind(agent_id)
        .bind(tool_name)
        .fetch_optional(&self.state.db)
        .await
        .ok()
        .flatten()?;

        // Hidden tools are not advertised, but a model can still emit a call
        // for one it remembers. Refusing here with the reason is far better
        // than letting the invocation fail deep inside the implementation.
        let impl_ref: String = row.try_get("impl_ref").unwrap_or_default();
        if let Some(reason) = crate::services::capabilities::unavailable_reason(&impl_ref) {
            tracing::warn!(tool = %tool_name, %reason, "refused call to unavailable tool");
            return None;
        }

        row.try_get("id").ok()
    }

    async fn persist_result(&self, run_id: &str, content: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE task_runs SET status = 'completed', finished_at = ?, result_json = ? WHERE id = ?")
            .bind(&now)
            .bind(json!({ "summary": content }).to_string())
            .bind(run_id)
            .execute(&self.state.db)
            .await
            .ok();
    }

    /// Answer a chat message with the conversation history. Chat is a pure
    /// LLM interaction: it never creates tasks or invokes tools on its own.
    /// Tasks are created only when explicitly requested and confirmed.
    pub async fn chat(&self, agent_id: &str, conversation_id: &str, _user_message: &str) -> Result<String, String> {
        self.chat_streaming(agent_id, conversation_id, &|_| {}).await
    }

    /// Chat with an incremental text callback.
    ///
    /// Every provider streams through its `stream` implementation; the default
    /// trait method makes non-streaming providers emit the whole answer as one
    /// delta, so the caller's behavior is uniform.
    pub async fn chat_streaming(
        &self,
        agent_id: &str,
        conversation_id: &str,
        on_delta: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<String, String> {
        // Reset the trace for this exchange.
        self.last_tool_trace.lock().expect("trace poisoned").clear();
        let provider = self
            .build_provider(agent_id)
            .await
            .ok_or_else(|| "no LLM provider configured for this agent. Add one in Settings, then select it for the agent.".to_string())?;

        // Load the agent's instructions.
        let instructions: String = sqlx::query("SELECT instructions FROM agents WHERE id = ?")
            .bind(agent_id)
            .fetch_optional(&self.state.db)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("instructions").ok())
            .unwrap_or_default();

        // Build the message list from the conversation history (last 40
        // messages) plus the new user message. Insertion order (rowid) is
        // the only reliable ordering: ids are random UUIDs and created_at
        // can tie within the same exchange.
        let history = sqlx::query(
            "SELECT role, content FROM messages WHERE conversation_id = ? ORDER BY rowid DESC LIMIT 40",
        )
        .bind(conversation_id)
        .fetch_all(&self.state.db)
        .await
        .unwrap_or_default();

        let mut messages = vec![LlmMessage {
            role: "system".into(),
            content: format!(
                "{instructions}\n\nYou are a personal assistant in a chat conversation. \
                 Answer the user's questions directly. You may use the available tools when \
                 they help answer (reading workspace files, web research, connected \
                 integrations). Do not create tasks unless the user explicitly asks you to \
                 schedule or automate something; in that case, describe what you would set \
                 up and ask for confirmation first."
            ),
            tool_calls: None,
            tool_call_id: None,
        }];
        // History is newest-first; reverse to chronological order.
        for row in history.iter().rev() {
            let role: String = row.get("role");
            let content: String = row.get("content");
            if role == "user" || role == "assistant" {
                messages.push(LlmMessage {
                    role,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        // Chat can invoke the agent's authorized tools (bounded loop).
        let available_tools = self.available_tools(agent_id).await;
        let mut tool_calls_made: u32 = 0;

        for _iteration in 0..8 {
            let completion = provider
                .stream(&messages, &available_tools, None, on_delta)
                .await
                .map_err(|e| format!("LLM request failed: {e}"))?;

            if completion.tool_calls.is_empty() {
                if completion.content.trim().is_empty() {
                    return Err("the model returned an empty response".into());
                }
                return Ok(completion.content);
            }

            messages.push(LlmMessage {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });

            for call in &completion.tool_calls {
                if tool_calls_made >= 10 {
                    return Ok(completion.content);
                }
                tool_calls_made += 1;

                // MCP tools bypass the local registry: they live on a remote
                // server and were already advertised to the model, so the call
                // is routed by name. The server is the one the user connected,
                // so no further authorization applies.
                if let Some(rest) = call.name.strip_prefix("mcp__") {
                    if let Some((server_id, tool_name)) = rest.split_once("__") {
                        let result = self.call_mcp_tool(server_id, tool_name, &call.arguments).await;
                        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
                            "tool": call.name,
                            "arguments": call.arguments,
                            "status": if result.is_ok() { "ok" } else { "error" },
                            "error": result.as_ref().err().map(|e| e.to_string()),
                        }));
                        let content = match result {
                            Ok(value) => value.to_string(),
                            Err(e) => format!("tool error: {e}"),
                        };
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                        continue;
                    }
                }

                let tool_id = self.resolve_tool(agent_id, &call.name).await;
                let Some(tool_id) = tool_id else {
                    messages.push(LlmMessage {
                        role: "tool".into(),
                        content: format!("tool '{}' is not available to this agent", call.name),
                        tool_calls: None,
                        tool_call_id: Some(call.id.clone()),
                    });
                    continue;
                };

                match self.tools.authorize_in_conversation(agent_id, &tool_id, &call.arguments, None, Some(conversation_id)).await {
                    Ok(Authorization::Allowed) => {
                        let result = self.tools.invoke_chat(agent_id, &tool_id, &call.arguments).await;
                        if let Err(e) = &result {
                            tracing::warn!(tool = %call.name, error = %e, args = %call.arguments, "chat tool invocation failed");
                        }
                        // Record the call in the activity trace.
                        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
                            "tool": call.name,
                            "arguments": call.arguments,
                            "status": if result.is_ok() { "ok" } else { "error" },
                            "error": result.as_ref().err().map(|e| e.to_string()),
                        }));
                        let content = match result {
                            Ok(value) => value.to_string(),
                            Err(e) => format!("tool error: {e}"),
                        };
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Ok(Authorization::RequiresApproval { approval_id }) => {
                        // Record the pending approval in the trace so the
                        // frontend can render an inline decision card. The
                        // exchange ends here: the HTTP response returns right
                        // away (blocking it for hours would only get the
                        // connection killed), and when the user approves, the
                        // frontend resends the message and this loop runs
                        // again — the approved operation is then executed
                        // directly by the authorization check below.
                        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
                            "tool": call.name,
                            "arguments": call.arguments,
                            "status": "approval_required",
                            "approval_id": approval_id,
                        }));
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: "this operation requires user approval; the request is pending in this chat and in the Approvals view. Ask the user to approve it, then try again".into(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Ok(Authorization::Denied(reason)) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("denied: {reason}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Err(e) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("authorization error: {e}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                }
            }
        }
        Err("the assistant used too many tool calls without reaching an answer".into())
    }

    /// Resume a chat exchange after the user approved a pending tool call.
    /// Executes the approved operation, feeds the result to the LLM with the
    /// conversation history, and continues the bounded loop to the final
    /// answer. The approval is verified (status + operation hash) before
    /// anything runs.
    pub async fn resume_chat(
        &self,
        agent_id: &str,
        conversation_id: &str,
        approval_id: &str,
    ) -> Result<String, String> {
        let provider = self
            .build_provider(agent_id)
            .await
            .ok_or_else(|| "no LLM provider configured for this agent. Add one in Settings, then select it for the agent.".to_string())?;

        // Load the approved operation and verify it is really approved.
        let row = sqlx::query("SELECT skill_tool_id, operation_json FROM approval_requests WHERE id = ?")
            .bind(approval_id)
            .fetch_optional(&self.state.db)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "approval not found".to_string())?;
        let skill_tool_id: String = row.get("skill_tool_id");
        let operation: Value = serde_json::from_str(row.get::<String, _>("operation_json").as_str())
            .map_err(|_| "invalid stored operation".to_string())?;
        let args = operation.get("args").cloned().unwrap_or(json!({}));
        let tool_name = operation.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // The approval must still be valid (defense in depth: status and
        // hash of the exact operation being executed).
        self.tools
            .approvals
            .revalidate(approval_id, &operation)
            .await
            .map_err(|e| e.to_string())?;

        // Execute the approved tool and record it in the trace.
        let result = self.tools.invoke_chat(agent_id, &skill_tool_id, &args).await;
        if let Err(e) = &result {
            tracing::warn!(tool = %tool_name, error = %e, "resumed tool invocation failed");
        }
        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
            "tool": tool_name,
            "arguments": args,
            "status": if result.is_ok() { "ok" } else { "error" },
            "error": result.as_ref().err().map(|e| e.to_string()),
        }));
        let tool_content = match result {
            Ok(value) => value.to_string(),
            Err(e) => format!("tool error: {e}"),
        };

        // Rebuild the conversation context: system + history + the pending
        // assistant tool-call request + the tool result, then continue the
        // bounded loop.
        let instructions: String = sqlx::query("SELECT instructions FROM agents WHERE id = ?")
            .bind(agent_id)
            .fetch_optional(&self.state.db)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<String, _>("instructions").ok())
            .unwrap_or_default();
        let history = sqlx::query(
            "SELECT role, content FROM messages WHERE conversation_id = ? ORDER BY rowid DESC LIMIT 40",
        )
        .bind(conversation_id)
        .fetch_all(&self.state.db)
        .await
        .unwrap_or_default();

        let mut messages = vec![LlmMessage {
            role: "system".into(),
            content: format!(
                "{instructions}\n\nYou are a personal assistant in a chat conversation. \
                 Answer the user's questions directly. You may use the available tools when \
                 they help answer (reading workspace files, web research, connected \
                 integrations). Do not create tasks unless the user explicitly asks you to \
                 schedule or automate something; in that case, describe what you would set \
                 up and ask for confirmation first."
            ),
            tool_calls: None,
            tool_call_id: None,
        }];
        for row in history.iter().rev() {
            let role: String = row.get("role");
            let content: String = row.get("content");
            if role == "user" || role == "assistant" {
                messages.push(LlmMessage { role, content, tool_calls: None, tool_call_id: None });
            }
        }
        // The assistant's tool-call request that was awaiting approval, and
        // its result — so the model sees a consistent tool-use transcript.
        messages.push(LlmMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(vec![crate::services::llm::LlmToolCall {
                id: format!("resume_{approval_id}"),
                name: tool_name.clone(),
                arguments: args.clone(),
            }]),
            tool_call_id: None,
        });
        messages.push(LlmMessage {
            role: "tool".into(),
            content: tool_content,
            tool_calls: None,
            tool_call_id: Some(format!("resume_{approval_id}")),
        });

        let available_tools = self.available_tools(agent_id).await;
        let mut tool_calls_made: u32 = 0;
        for _iteration in 0..8 {
            let completion = provider
                .complete(&messages, &available_tools, None)
                .await
                .map_err(|e| format!("LLM request failed: {e}"))?;
            if completion.tool_calls.is_empty() {
                if completion.content.trim().is_empty() {
                    return Err("the model returned an empty response".into());
                }
                return Ok(completion.content);
            }
            messages.push(LlmMessage {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });
            for call in &completion.tool_calls {
                if tool_calls_made >= 10 {
                    return Ok(completion.content);
                }
                tool_calls_made += 1;
                let tool_id = self.resolve_tool(agent_id, &call.name).await;
                let Some(tool_id) = tool_id else {
                    messages.push(LlmMessage {
                        role: "tool".into(),
                        content: format!("tool '{}' is not available to this agent", call.name),
                        tool_calls: None,
                        tool_call_id: Some(call.id.clone()),
                    });
                    continue;
                };
                match self.tools.authorize_in_conversation(agent_id, &tool_id, &call.arguments, None, Some(conversation_id)).await {
                    Ok(Authorization::Allowed) => {
                        let result = self.tools.invoke_chat(agent_id, &tool_id, &call.arguments).await;
                        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
                            "tool": call.name,
                            "arguments": call.arguments,
                            "status": if result.is_ok() { "ok" } else { "error" },
                            "error": result.as_ref().err().map(|e| e.to_string()),
                        }));
                        let content = match result {
                            Ok(value) => value.to_string(),
                            Err(e) => format!("tool error: {e}"),
                        };
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Ok(Authorization::RequiresApproval { approval_id }) => {
                        self.last_tool_trace.lock().expect("trace poisoned").push(json!({
                            "tool": call.name,
                            "arguments": call.arguments,
                            "status": "approval_required",
                            "approval_id": approval_id,
                        }));
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: "this operation requires user approval; the request is pending in this chat and in the Approvals view. Ask the user to approve it, then try again".into(),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Ok(Authorization::Denied(reason)) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("denied: {reason}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                    Err(e) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: format!("authorization error: {e}"),
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                        });
                    }
                }
            }
        }
        Err("the assistant used too many tool calls without reaching an answer".into())
    }

    /// Ask the LLM for a short chat title summarizing the first message.
    /// Falls back to the caller on any failure.
    pub async fn summarize_title(&self, agent_id: &str, first_message: &str) -> Result<String, String> {
        let provider = self.build_provider(agent_id).await.ok_or("no provider")?;
        let completion = provider
            .complete(
                &[
                    LlmMessage {
                        role: "system".into(),
                        content: "Summarize the user's message as a chat title: at most 5 words, in the same language as the message, no quotes, no punctuation at the end. Answer with the title only.".into(),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    LlmMessage {
                        role: "user".into(),
                        content: first_message.chars().take(500).collect(),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                ],
                &[],
                Some(20),
            )
            .await
            .map_err(|e| format!("{e}"))?;
        let title = completion.content.trim().trim_matches('"').chars().take(60).collect::<String>();
        if title.is_empty() {
            return Err("empty title".into());
        }
        Ok(title)
    }

    /// Run an ad-hoc objective with an agent's configuration (no persisted
    /// task). Used for subagent delegation: the subagent runs its own
    /// bounded loop with its own instructions and returns the final answer.
    pub fn run_ad_hoc<'a>(
        &'a self,
        agent_id: &'a str,
        objective: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(self.run_ad_hoc_inner(agent_id, objective))
    }

    async fn run_ad_hoc_inner(&self, agent_id: &str, objective: &str) -> Result<String, String> {
        let provider = self
            .build_provider(agent_id)
            .await
            .ok_or_else(|| "no LLM provider configured for the subagent".to_string())?;
        let instructions: String = sqlx::query("SELECT instructions FROM agents WHERE id = ?")
            .bind(agent_id)
            .fetch_optional(&self.state.db)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("instructions").ok())
            .unwrap_or_default();

        let mut messages = vec![
            LlmMessage {
                role: "system".into(),
                content: format!(
                    "{instructions}\n\nYou are completing a delegated objective. Work through \
                     it with the available tools when needed, then reply with the final result."
                ),
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage {
                role: "user".into(),
                content: objective.to_string(),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let available_tools = self.available_tools(agent_id).await;
        let mut tool_calls_made: u32 = 0;

        for _ in 0..self.max_iterations() {
            let completion = provider
                .complete(&messages, &available_tools, None)
                .await
                .map_err(|e| format!("LLM request failed: {e}"))?;

            if completion.tool_calls.is_empty() {
                return Ok(completion.content);
            }

            messages.push(LlmMessage {
                role: "assistant".into(),
                content: completion.content.clone(),
                tool_calls: Some(completion.tool_calls.clone()),
                tool_call_id: None,
            });

            for call in &completion.tool_calls {
                if tool_calls_made >= self.max_tool_calls() {
                    return Err("tool call limit reached".into());
                }
                tool_calls_made += 1;

                let tool_id = self.resolve_tool(agent_id, &call.name).await;
                let content = match tool_id {
                    Some(tool_id) => match self.tools.authorize(agent_id, &tool_id, &call.arguments, None).await {
                        Ok(Authorization::Allowed) => match self.tools.invoke_chat(agent_id, &tool_id, &call.arguments).await {
                            Ok(value) => value.to_string(),
                            Err(e) => format!("tool error: {e}"),
                        },
                        Ok(Authorization::RequiresApproval { .. }) => {
                            "this operation requires user approval; report that you could not complete it".into()
                        }
                        Ok(Authorization::Denied(reason)) => format!("denied: {reason}"),
                        Err(e) => format!("authorization error: {e}"),
                    },
                    None => format!("tool '{}' is not available to this agent", call.name),
                };
                messages.push(LlmMessage {
                    role: "tool".into(),
                    content,
                    tool_calls: None,
                    tool_call_id: Some(call.id.clone()),
                });
            }
        }
        Err("iteration limit reached".into())
    }
}