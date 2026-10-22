//! Task management tools (spec part1 section 6/7): the agent creates,
//! updates, runs, and deletes tasks through the same validated TaskService
//! the REST API uses — never through direct database writes.

use serde_json::{json, Value};
use sqlx::Row;

use crate::{error::ApiError, state::AppState};

/// Execute a task-management builtin.
pub async fn execute_task_tool(
    state: &AppState,
    operation: &str,
    args: &Value,
    user_id: &str,
) -> Result<Value, ApiError> {
    let service = crate::services::task::TaskService::new(state);
    match operation {
        "task_create" => {
            let agent_id = args
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let agent_id = if agent_id.is_empty() { default_agent_id(state, user_id).await } else { agent_id };
            let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("title is required".into())
            })?;
            let objective = args.get("objective").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("objective is required".into())
            })?;
            let schedule = args.get("schedule").filter(|s| !s.is_null());
            let timezone = args.get("timezone").and_then(|v| v.as_str()).unwrap_or("UTC");
            let task_id = service
                .create(user_id, &agent_id, title, objective, schedule, timezone, None)
                .await?;
            Ok(json!({
                "task_id": task_id,
                "created": true,
                "title": title,
                "schedule": schedule.cloned().unwrap_or(json!(null)),
                "note": if schedule.is_some() {
                    "the task is active and will run on the schedule"
                } else {
                    "the task was created without a schedule; run it manually or add one"
                },
            }))
        }
        "task_update" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("task_id is required".into())
            })?;
            service
                .update(
                    user_id,
                    task_id,
                    args.get("title").and_then(|v| v.as_str()),
                    args.get("objective").and_then(|v| v.as_str()),
                    args.get("schedule").filter(|s| !s.is_null()),
                )
                .await?;
            Ok(json!({ "task_id": task_id, "updated": true }))
        }
        "task_delete" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("task_id is required".into())
            })?;
            service.delete(user_id, task_id).await?;
            Ok(json!({ "task_id": task_id, "deleted": true }))
        }
        "task_list" => {
            let rows = sqlx::query("SELECT id, title, status, schedule_json FROM tasks WHERE user_id = ? ORDER BY updated_at DESC LIMIT 50")
                .bind(user_id)
                .fetch_all(&state.db)
                .await?;
            let items: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "task_id": r.get::<String, _>("id"),
                        "title": r.get::<String, _>("title"),
                        "status": r.get::<String, _>("status"),
                        "schedule": r.get::<Option<String>, _>("schedule_json")
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                    })
                })
                .collect();
            Ok(json!({ "tasks": items }))
        }
        "task_run" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("task_id is required".into())
            })?;
            let run_id = service.request_run(user_id, task_id).await?;
            Ok(json!({ "task_id": task_id, "task_run_id": run_id, "queued": true }))
        }
        "task_pause" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("task_id is required".into())
            })?;
            service.set_status(user_id, task_id, "paused").await?;
            Ok(json!({ "task_id": task_id, "status": "paused" }))
        }
        "task_resume" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("task_id is required".into())
            })?;
            service.set_status(user_id, task_id, "active").await?;
            Ok(json!({ "task_id": task_id, "status": "active" }))
        }
        // Subagent delegation: the agent creates a specialized subagent and
        // delegates work to it. The subagent runs its own bounded LLM loop
        // with the delegated objective and returns the result.
        "subagent_create" => {
            let name = args.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                ApiError::Unprocessable("name is required".into())
            })?;
            let instructions = args.get("instructions").and_then(|v| v.as_str()).unwrap_or("");
            // Subagents inherit the creating agent's LLM provider and model
            // so they can execute without extra configuration.
            let parent = match args.get("parent_agent_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => default_agent_id(state, user_id).await,
            };
            let (provider_id, model_id): (Option<String>, Option<String>) = sqlx::query(
                "SELECT llm_provider_id, model_id FROM agents WHERE id = ?",
            )
            .bind(&parent)
            .fetch_optional(&state.db)
            .await?
            .map(|r| (r.get("llm_provider_id"), r.get("model_id")))
            .unwrap_or((None, None));
            let id = uuid::Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO agents (id, user_id, name, instructions, llm_provider_id, model_id, status, settings_json, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, 'active', '{}', ?, ?)",
            )
            .bind(&id)
            .bind(user_id)
            .bind(name.trim())
            .bind(instructions)
            .bind(&provider_id)
            .bind(&model_id)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&state.db)
            .await?;
            Ok(json!({ "subagent_id": id, "name": name, "created": true }))
        }
        "subagent_delegate" => {
            let subagent_id = args.get("subagent_id").and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::Unprocessable("subagent_id is required".into()))?;
            let objective = args.get("objective").and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::Unprocessable("objective is required".into()))?;
            // The subagent must belong to the same user.
            let owner: Option<String> = sqlx::query("SELECT user_id FROM agents WHERE id = ?")
                .bind(subagent_id)
                .fetch_optional(&state.db)
                .await?
                .map(|r| r.get("user_id"));
            if owner.as_deref() != Some(user_id) {
                return Err(ApiError::NotFound);
            }
            // Run the subagent's bounded loop directly (synchronous within
            // the parent's execution; the parent waits for the result).
            let runtime = crate::services::agent_runtime::AgentRuntime::new(state.clone());
            let result = runtime
                .run_ad_hoc(subagent_id, objective)
                .await
                .map_err(ApiError::Unprocessable)?;
            // Optional cleanup policy: delete the subagent after use so
            // one-off helpers do not accumulate.
            if auto_delete_subagents(state) {
                sqlx::query("DELETE FROM agents WHERE id = ?")
                    .bind(subagent_id)
                    .execute(&state.db)
                    .await?;
            }
            Ok(json!({ "subagent_id": subagent_id, "result": result }))
        }
        "subagent_list" => {
            let rows = sqlx::query("SELECT id, name, instructions, status FROM agents WHERE user_id = ? ORDER BY created_at")
                .bind(user_id)
                .fetch_all(&state.db)
                .await?;
            Ok(json!({
                "agents": rows.iter().map(|r| json!({
                    "agent_id": r.get::<String, _>("id"),
                    "name": r.get::<String, _>("name"),
                    "instructions": r.get::<String, _>("instructions"),
                    "status": r.get::<String, _>("status"),
                })).collect::<Vec<_>>()
            }))
        }
        "subagent_delete" => {
            let subagent_id = args.get("subagent_id").and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::Unprocessable("subagent_id is required".into()))?;
            // The subagent must belong to the same user.
            let owner: Option<String> = sqlx::query("SELECT user_id FROM agents WHERE id = ?")
                .bind(subagent_id)
                .fetch_optional(&state.db)
                .await?
                .map(|r| r.get("user_id"));
            if owner.as_deref() != Some(user_id) {
                return Err(ApiError::NotFound);
            }
            sqlx::query("DELETE FROM agents WHERE id = ?")
                .bind(subagent_id)
                .execute(&state.db)
                .await?;
            Ok(json!({ "subagent_id": subagent_id, "deleted": true }))
        }
        other => Err(ApiError::Unprocessable(format!("unknown task operation '{other}'"))),
    }
}

/// Whether subagents should be deleted after a successful delegation
/// (spec: avoid accumulating unused subagents). Configurable via
/// `AUREN_SUBAGENT_AUTO_DELETE` (default: keep them for reuse).
fn auto_delete_subagents(state: &AppState) -> bool {
    state
        .config
        .subagent_auto_delete
}

/// The user's first agent, used when the tool call does not name one.
async fn default_agent_id(state: &AppState, user_id: &str) -> String {
    sqlx::query("SELECT id FROM agents WHERE user_id = ? ORDER BY created_at LIMIT 1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get("id").ok())
        .unwrap_or_default()
}
