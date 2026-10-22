//! Bounded agent execution loop (spec section 12).
//!
//! The runtime loads the task, plans with the LLM, validates and authorizes
//! each requested action through the permission engine, executes tools, and
//! persists state after every transition. Limits prevent unbounded loops.

use serde_json::json;
use sqlx::Row;

use crate::{
    services::{
        llm::{LlmMessage, LlmProvider, LlmToolDefinition},
        tool_registry::{Authorization, ToolRegistry},
    },
    state::AppState,
};

/// Execution limits (spec section 12).
const MAX_ITERATIONS: u32 = 25;
const MAX_TOOL_CALLS: u32 = 50;
const MAX_LLM_CALLS: u32 = 40;

pub struct AgentRuntime {
    state: AppState,
    tools: ToolRegistry,
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
        Self { state, tools }
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

        for llm_calls in 0..MAX_ITERATIONS {
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
            if llm_calls >= MAX_LLM_CALLS {
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
                if tool_calls_made >= MAX_TOOL_CALLS {
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

    /// Tools available to the agent from enabled skills.
    async fn available_tools(&self, agent_id: &str) -> Vec<LlmToolDefinition> {
        let rows = sqlx::query(
            "SELECT st.tool_name, st.description, st.input_schema_json
             FROM skill_tools st
             JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             JOIN skill_versions sv ON sv.id = st.skill_version_id
             JOIN skills sk ON sk.id = sv.skill_id
             WHERE ask.agent_id = ? AND ask.enabled = 1 AND sk.status = 'enabled'",
        )
        .bind(agent_id)
        .fetch_all(&self.state.db)
        .await
        .unwrap_or_default();
        rows.iter()
            .map(|r| LlmToolDefinition {
                name: r.get("tool_name"),
                description: r.get("description"),
                input_schema: serde_json::from_str(&r.get::<String, _>("input_schema_json")).unwrap_or(json!({})),
            })
            .collect()
    }

    async fn resolve_tool(&self, agent_id: &str, tool_name: &str) -> Option<String> {
        let row = sqlx::query(
            "SELECT st.id FROM skill_tools st
             JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             JOIN skills sk ON sk.id = (SELECT skill_id FROM skill_versions WHERE id = st.skill_version_id)
             WHERE ask.agent_id = ? AND st.tool_name = ? AND ask.enabled = 1 AND sk.status = 'enabled'",
        )
        .bind(agent_id)
        .bind(tool_name)
        .fetch_optional(&self.state.db)
        .await
        .ok()
        .flatten()?;
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
        // messages) plus the new user message.
        let history = sqlx::query(
            "SELECT role, content FROM messages WHERE conversation_id = ? ORDER BY created_at DESC LIMIT 40",
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

                match self.tools.authorize(agent_id, &tool_id, &call.arguments, None).await {
                    Ok(Authorization::Allowed) => {
                        let result = self.tools.invoke_chat(agent_id, &tool_id, &call.arguments).await;
                        if let Err(e) = &result {
                            tracing::warn!(tool = %call.name, error = %e, args = %call.arguments, "chat tool invocation failed");
                        }
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
                    Ok(Authorization::RequiresApproval { .. }) => {
                        messages.push(LlmMessage {
                            role: "tool".into(),
                            content: "this operation requires user approval; tell the user to approve it in the Approvals view, then try again".into(),
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
}