//! Memory, notifications, workspace, audit, and system status APIs
//! (spec sections 3.12-3.15).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::{auth_user::AuthUser, pagination},
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/memories", memories_router())
        .nest("/notifications", notifications_router())
        .nest("/workspace", workspace_router())
        .nest("/audit-events", audit_router())
        .route("/system/status", get(system_status))
}

// ---------------------------------------------------------------------------
// Memory (3.12)
// ---------------------------------------------------------------------------

pub fn memories_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_memories).post(create_memory))
        .route("/{memory_id}", get(get_memory).delete(delete_memory))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateMemoryRequest {
    pub category: String,
    pub content: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
}

pub async fn create_memory(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateMemoryRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if req.content.trim().is_empty() {
        return Err(ApiError::Unprocessable("content is required".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO memories (id, user_id, agent_id, task_id, category, content, source_json, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, '{}', ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&req.agent_id)
    .bind(&req.task_id)
    .bind(&req.category)
    .bind(req.content.trim())
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id, "category": req.category }))))
}

pub async fn list_memories(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let mut sql = String::from("SELECT * FROM memories WHERE user_id = ?");
    if params.category.is_some() {
        sql.push_str(" AND category = ?");
    }
    sql.push_str(" AND id > ? ORDER BY id LIMIT ?");
    let mut query = sqlx::query(&sql).bind(&user.id);
    if let Some(c) = &params.category {
        query = query.bind(c);
    }
    query = query.bind(cursor.clone().unwrap_or_default()).bind(limit + 1);
    let rows = query.fetch_all(&state.db).await?;
    let has_more = rows.len() as i64 > limit;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "category": r.get::<String, _>("category"),
                "content": r.get::<String, _>("content"),
                "agent_id": r.get::<Option<String>, _>("agent_id"),
                "task_id": r.get::<Option<String>, _>("task_id"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();
    let next_cursor = if has_more {
        items.last().and_then(|i| i["id"].as_str()).map(pagination::encode_cursor)
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

async fn require_owned_memory(state: &AppState, user: &AuthUser, memory_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM memories WHERE id = ?")
        .bind(memory_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_memory(
    State(state): State<AppState>,
    user: AuthUser,
    Path(memory_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_memory(&state, &user, &memory_id).await?;
    let row = sqlx::query("SELECT * FROM memories WHERE id = ?").bind(&memory_id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "category": row.get::<String, _>("category"),
        "content": row.get::<String, _>("content"),
        "created_at": row.get::<String, _>("created_at"),
    })))
}

pub async fn delete_memory(
    State(state): State<AppState>,
    user: AuthUser,
    Path(memory_id): Path<String>,
) -> ApiResult<StatusCode> {
    require_owned_memory(&state, &user, &memory_id).await?;
    sqlx::query("DELETE FROM memories WHERE id = ?").bind(&memory_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Notifications (3.13)
// ---------------------------------------------------------------------------

pub fn notifications_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_notifications))
        .route("/read-all", post(read_all_notifications))
        .route("/{notification_id}", get(get_notification).delete(delete_notification))
        .route("/{notification_id}/read", post(mark_read))
}

pub async fn list_notifications(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM notifications WHERE user_id = ? AND id > ? ORDER BY id LIMIT ?")
                .bind(&user.id).bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM notifications WHERE user_id = ? ORDER BY id LIMIT ?")
                .bind(&user.id).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "notification_type": r.get::<String, _>("notification_type"),
                "title": r.get::<String, _>("title"),
                "body": r.get::<String, _>("body"),
                "read_at": r.get::<Option<String>, _>("read_at"),
                "task_id": r.get::<Option<String>, _>("task_id"),
                "task_run_id": r.get::<Option<String>, _>("task_run_id"),
                "approval_request_id": r.get::<Option<String>, _>("approval_request_id"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();
    let next_cursor = if has_more {
        items.last().and_then(|i| i["id"].as_str()).map(pagination::encode_cursor)
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

async fn require_owned_notification(state: &AppState, user: &AuthUser, notification_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM notifications WHERE id = ?")
        .bind(notification_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_notification(
    State(state): State<AppState>,
    user: AuthUser,
    Path(notification_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_notification(&state, &user, &notification_id).await?;
    let row = sqlx::query("SELECT * FROM notifications WHERE id = ?")
        .bind(&notification_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "notification_type": row.get::<String, _>("notification_type"),
        "title": row.get::<String, _>("title"),
        "body": row.get::<String, _>("body"),
        "read_at": row.get::<Option<String>, _>("read_at"),
        "created_at": row.get::<String, _>("created_at"),
    })))
}

/// Mark a notification as read. Reading never authorizes an operation
/// (spec 3.13: approval decisions use the approval endpoints).
pub async fn mark_read(
    State(state): State<AppState>,
    user: AuthUser,
    Path(notification_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_notification(&state, &user, &notification_id).await?;
    sqlx::query("UPDATE notifications SET read_at = ? WHERE id = ? AND read_at IS NULL")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&notification_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "id": notification_id, "read": true })))
}

pub async fn read_all_notifications(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query("UPDATE notifications SET read_at = ? WHERE user_id = ? AND read_at IS NULL")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&user.id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "updated": result.rows_affected() })))
}

pub async fn delete_notification(
    State(state): State<AppState>,
    user: AuthUser,
    Path(notification_id): Path<String>,
) -> ApiResult<StatusCode> {
    require_owned_notification(&state, &user, &notification_id).await?;
    sqlx::query("DELETE FROM notifications WHERE id = ?")
        .bind(&notification_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Workspace (3.14)
// ---------------------------------------------------------------------------

pub fn workspace_router() -> Router<AppState> {
    Router::new()
        .route("/config", get(get_workspace_config).patch(update_workspace_config))
        .route("/files", get(list_files).delete(delete_file))
        .route("/files/content", get(read_file).put(write_file))
}

/// The single authorized workspace root. Every file operation resolves
/// against this root and rejects traversal outside it (spec 3.14).
fn workspace_root(state: &AppState) -> std::path::PathBuf {
    std::path::PathBuf::from(&state.config.skills_dir)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("data"))
        .join("workspace")
}

/// Resolve a relative path against the root, rejecting traversal.
fn resolve_in_root(root: &std::path::Path, relative: &str) -> ApiResult<std::path::PathBuf> {
    let relative = relative.trim_start_matches('/');
    if relative.is_empty() {
        return Err(ApiError::Unprocessable("path is required".into()));
    }
    let resolved = root.join(relative);
    let canonical_root = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    // Reject `..` components before touching the filesystem.
    for component in resolved.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ApiError::Forbidden("path traversal is not allowed".into()));
        }
    }
    let canonical = resolved
        .canonicalize()
        .unwrap_or_else(|_| resolved.clone());
    if !canonical.starts_with(&canonical_root) {
        return Err(ApiError::Forbidden("path is outside the authorized root".into()));
    }
    Ok(canonical)
}

pub async fn get_workspace_config(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = 'workspace_config'")
        .fetch_optional(&state.db)
        .await?;
    let config = row
        .map(|r| {
            let v: String = r.get("value_json");
            serde_json::from_str::<serde_json::Value>(&v).unwrap_or(json!({}))
        })
        .unwrap_or_else(|| json!({ "authorized_roots": ["workspace"] }));
    Ok(Json(config))
}

#[derive(Deserialize)]
pub struct UpdateWorkspaceConfigRequest {
    pub config: serde_json::Value,
}

pub async fn update_workspace_config(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<UpdateWorkspaceConfigRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query(
        "INSERT INTO app_settings (key, value_json, updated_at) VALUES ('workspace_config', ?, ?)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(req.config.to_string())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.db)
    .await?;
    Ok(Json(req.config))
}

#[derive(Deserialize)]
pub struct FileParams {
    pub path: String,
}

pub async fn list_files(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<FileParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = workspace_root(&state);
    let dir = if params.path.is_empty() { root.clone() } else { resolve_in_root(&root, &params.path)? };
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            items.push(json!({ "name": name, "type": if is_dir { "directory" } else { "file" }, "size": size }));
        }
    }
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

pub async fn read_file(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<FileParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = workspace_root(&state);
    let path = resolve_in_root(&root, &params.path)?;
    let content = std::fs::read(&path)
        .map_err(|_| ApiError::NotFound)?;
    let content = String::from_utf8_lossy(&content).to_string();
    Ok(Json(json!({ "path": params.path, "content": content })))
}

#[derive(Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: String,
}

pub async fn write_file(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<WriteFileRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let root = workspace_root(&state);
    let path = resolve_in_root(&root, &req.path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    }
    std::fs::write(&path, &req.content).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    Ok(Json(json!({ "path": req.path, "bytes_written": req.content.len() })))
}

pub async fn delete_file(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<FileParams>,
) -> ApiResult<StatusCode> {
    let root = workspace_root(&state);
    let path = resolve_in_root(&root, &params.path)?;
    if path.is_dir() {
        std::fs::remove_dir(&path).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    } else {
        std::fs::remove_file(&path).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Audit (3.15)
// ---------------------------------------------------------------------------

pub fn audit_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_audit_events))
        .route("/{event_id}", get(get_audit_event))
}

pub async fn list_audit_events(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM audit_events WHERE id > ? ORDER BY id LIMIT ?")
                .bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM audit_events ORDER BY id LIMIT ?")
                .bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "task_run_id": r.get::<Option<String>, _>("task_run_id"),
                "event_type": r.get::<String, _>("event_type"),
                "actor_type": r.get::<String, _>("actor_type"),
                "actor_id": r.get::<Option<String>, _>("actor_id"),
                "details": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>("details_json")).unwrap_or(json!({})),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();
    let next_cursor = if has_more {
        items.last().and_then(|i| i["id"].as_str()).map(pagination::encode_cursor)
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

pub async fn get_audit_event(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(event_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT * FROM audit_events WHERE id = ?")
        .bind(&event_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "task_run_id": row.get::<Option<String>, _>("task_run_id"),
        "event_type": row.get::<String, _>("event_type"),
        "actor_type": row.get::<String, _>("actor_type"),
        "actor_id": row.get::<Option<String>, _>("actor_id"),
        "details": serde_json::from_str::<serde_json::Value>(&row.get::<String, _>("details_json")).unwrap_or(json!({})),
        "created_at": row.get::<String, _>("created_at"),
    })))
}

// ---------------------------------------------------------------------------
// System status (3.15)
// ---------------------------------------------------------------------------

/// Authenticated operational status. No credentials or internal details.
pub async fn system_status(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let db_ok = sqlx::query("SELECT 1").fetch_optional(&state.db).await.is_ok();
    let pending_jobs: i64 = sqlx::query("SELECT COUNT(*) AS c FROM queued_jobs WHERE status = 'pending'")
        .fetch_one(&state.db)
        .await?
        .get("c");
    let pending_approvals: i64 = sqlx::query("SELECT COUNT(*) AS c FROM approval_requests WHERE status = 'pending'")
        .fetch_one(&state.db)
        .await?
        .get("c");
    Ok(Json(json!({
        "status": if db_ok { "operational" } else { "degraded" },
        "database": db_ok,
        "pending_jobs": pending_jobs,
        "pending_approvals": pending_approvals,
        "user_id": user.id,
    })))
}