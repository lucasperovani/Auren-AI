//! Agents API (spec section 3.4).

use axum::{
    extract::{Path, State},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_agents).post(create_agent))
        .route("/{agent_id}", get(get_agent).patch(update_agent).delete(delete_agent))
        .route("/{agent_id}/pause", post(pause_agent))
        .route("/{agent_id}/resume", post(resume_agent))
        .route("/{agent_id}/skills", get(list_agent_skills))
        .route("/{agent_id}/skills/{skill_id}", put(set_agent_skill))
        .route("/{agent_id}/permissions", get(get_permissions).put(put_permissions))
}

#[derive(Deserialize)]
pub struct CreateAgentRequest {
    pub name: String,
    #[serde(default)]
    pub instructions: String,
    pub llm_provider_id: Option<String>,
    pub model_id: Option<String>,
}

#[derive(Serialize)]
pub struct AgentResponse {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub llm_provider_id: Option<String>,
    pub model_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_agent(r: &sqlx::sqlite::SqliteRow) -> AgentResponse {
    AgentResponse {
        id: r.get("id"),
        name: r.get("name"),
        instructions: r.get("instructions"),
        llm_provider_id: r.get("llm_provider_id"),
        model_id: r.get("model_id"),
        status: r.get("status"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn list_agents(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query("SELECT * FROM agents WHERE user_id = ? ORDER BY created_at")
        .bind(&user.id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<AgentResponse> = rows.iter().map(row_to_agent).collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

pub async fn create_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateAgentRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<AgentResponse>)> {
    if req.name.trim().is_empty() {
        return Err(ApiError::Unprocessable("name is required".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO agents (id, user_id, name, instructions, llm_provider_id, model_id, status, settings_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 'active', '{}', ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(req.name.trim())
    .bind(&req.instructions)
    .bind(&req.llm_provider_id)
    .bind(&req.model_id)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;

    let row = sqlx::query("SELECT * FROM agents WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    Ok((axum::http::StatusCode::CREATED, Json(row_to_agent(&row))))
}

async fn require_owned_agent(state: &AppState, user: &AuthUser, agent_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM agents WHERE id = ?")
        .bind(agent_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<AgentResponse>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    let row = sqlx::query("SELECT * FROM agents WHERE id = ?")
        .bind(&agent_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_agent(&row)))
}

#[derive(Deserialize)]
pub struct UpdateAgentRequest {
    pub name: Option<String>,
    pub instructions: Option<String>,
    pub llm_provider_id: Option<String>,
    pub model_id: Option<String>,
}

pub async fn update_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
    Json(req): Json<UpdateAgentRequest>,
) -> ApiResult<Json<AgentResponse>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(n) = &req.name {
        if n.trim().is_empty() {
            return Err(ApiError::Unprocessable("name cannot be empty".into()));
        }
        sqlx::query("UPDATE agents SET name = ?, updated_at = ? WHERE id = ?")
            .bind(n.trim()).bind(&now).bind(&agent_id).execute(&state.db).await?;
    }
    if let Some(i) = &req.instructions {
        sqlx::query("UPDATE agents SET instructions = ?, updated_at = ? WHERE id = ?")
            .bind(i).bind(&now).bind(&agent_id).execute(&state.db).await?;
    }
    if req.llm_provider_id.is_some() || req.model_id.is_some() {
        sqlx::query("UPDATE agents SET llm_provider_id = COALESCE(?, llm_provider_id), model_id = COALESCE(?, model_id), updated_at = ? WHERE id = ?")
            .bind(&req.llm_provider_id).bind(&req.model_id).bind(&now).bind(&agent_id).execute(&state.db).await?;
    }
    let row = sqlx::query("SELECT * FROM agents WHERE id = ?")
        .bind(&agent_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_agent(&row)))
}

/// Delete (archive) an agent. Tasks referencing the agent are protected by
/// the RESTRICT foreign key, so archival is the safe path.
pub async fn delete_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    require_owned_agent(&state, &user, &agent_id).await?;
    let has_tasks: i64 = sqlx::query("SELECT COUNT(*) AS c FROM tasks WHERE agent_id = ?")
        .bind(&agent_id)
        .fetch_one(&state.db)
        .await?
        .get("c");
    if has_tasks > 0 {
        sqlx::query("UPDATE agents SET status = 'archived', updated_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&agent_id)
            .execute(&state.db)
            .await?;
        return Ok(axum::http::StatusCode::NO_CONTENT);
    }
    sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(&agent_id)
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn pause_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    sqlx::query("UPDATE agents SET status = 'paused', updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&agent_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "status": "paused" })))
}

pub async fn resume_agent(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    sqlx::query("UPDATE agents SET status = 'active', updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&agent_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "status": "active" })))
}

pub async fn list_agent_skills(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    let rows = sqlx::query(
        "SELECT sk.id, sk.name, sk.status, sv.version, ask.enabled
         FROM agent_skills ask
         JOIN skill_versions sv ON sv.id = ask.skill_version_id
         JOIN skills sk ON sk.id = sv.skill_id
         WHERE ask.agent_id = ?",
    )
    .bind(&agent_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "skill_id": r.get::<String, _>("id"),
                "name": r.get::<String, _>("name"),
                "status": r.get::<String, _>("status"),
                "version": r.get::<String, _>("version"),
                "enabled": r.get::<i64, _>("enabled") == 1,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct SetAgentSkillRequest {
    pub enabled: bool,
}

pub async fn set_agent_skill(
    State(state): State<AppState>,
    user: AuthUser,
    Path((agent_id, skill_id)): Path<(String, String)>,
    Json(req): Json<SetAgentSkillRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    // Resolve the skill's active version.
    let row = sqlx::query("SELECT active_version_id FROM skills WHERE id = ?")
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let version_id: Option<String> = row.get("active_version_id");
    let version_id = version_id.ok_or_else(|| ApiError::Conflict("skill has no active version".into()))?;

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(agent_id, skill_version_id) DO UPDATE SET enabled = excluded.enabled",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&agent_id)
    .bind(&version_id)
    .bind(req.enabled as i64)
    .bind(&now)
    .execute(&state.db)
    .await?;
    Ok(Json(json!({ "skill_id": skill_id, "enabled": req.enabled })))
}

pub async fn get_permissions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    let rows = sqlx::query(
        "SELECT atp.skill_tool_id, atp.policy, st.tool_name
         FROM agent_tool_permissions atp
         JOIN skill_tools st ON st.id = atp.skill_tool_id
         WHERE atp.agent_id = ?",
    )
    .bind(&agent_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "skill_tool_id": r.get::<String, _>("skill_tool_id"),
                "tool_name": r.get::<String, _>("tool_name"),
                "policy": r.get::<String, _>("policy"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct PutPermissionsRequest {
    pub permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize)]
pub struct PermissionEntry {
    pub skill_tool_id: String,
    pub policy: String,
}

pub async fn put_permissions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(agent_id): Path<String>,
    Json(req): Json<PutPermissionsRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_agent(&state, &user, &agent_id).await?;
    for entry in &req.permissions {
        if !matches!(entry.policy.as_str(), "allow" | "deny" | "ask") {
            return Err(ApiError::Unprocessable(format!(
                "invalid policy '{}': must be allow, deny, or ask",
                entry.policy
            )));
        }
        // The tool must belong to a skill version enabled for this agent.
        let valid: Option<sqlx::sqlite::SqliteRow> = sqlx::query(
            "SELECT 1 FROM skill_tools st JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             WHERE st.id = ? AND ask.agent_id = ?",
        )
        .bind(&entry.skill_tool_id)
        .bind(&agent_id)
        .fetch_optional(&state.db)
        .await?;
        if valid.is_none() {
            return Err(ApiError::NotFound);
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.db.begin().await.map_err(ApiError::from)?;
    for entry in &req.permissions {
        sqlx::query(
            "INSERT INTO agent_tool_permissions (id, agent_id, skill_tool_id, policy, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(agent_id, skill_tool_id) DO UPDATE SET policy = excluded.policy, updated_at = excluded.updated_at",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&agent_id)
        .bind(&entry.skill_tool_id)
        .bind(&entry.policy)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await.map_err(ApiError::from)?;
    Ok(Json(json!({ "updated": req.permissions.len() })))
}