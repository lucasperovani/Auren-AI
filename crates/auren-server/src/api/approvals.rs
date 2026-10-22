//! Approval API (spec section 3.11).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::{auth_user::AuthUser, pagination},
    error::{ApiError, ApiResult},
    services::approvals::ApprovalService,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_approvals))
        .route("/{approval_id}", get(get_approval))
        .route("/{approval_id}/approve", post(approve))
        .route("/{approval_id}/reject", post(reject))
        .route("/{approval_id}/cancel", post(cancel))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub status: Option<String>,
}

pub async fn list_approvals(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);

    let mut sql = String::from(
        "SELECT a.* FROM approval_requests a
         LEFT JOIN task_runs r ON r.id = a.task_run_id
         LEFT JOIN tasks t ON t.id = r.task_id
         WHERE (t.user_id = ? OR a.task_run_id IS NULL)",
    );
    if params.status.is_some() {
        sql.push_str(" AND a.status = ?");
    }
    sql.push_str(" AND a.id > ? ORDER BY a.id LIMIT ?");
    let mut query = sqlx::query(&sql).bind(&user.id);
    if let Some(s) = &params.status {
        query = query.bind(s);
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
                "task_run_id": r.get::<Option<String>, _>("task_run_id"),
                "status": r.get::<String, _>("status"),
                "expires_at": r.get::<String, _>("expires_at"),
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

/// Retrieve the complete operation details of an approval.
pub async fn get_approval(
    State(state): State<AppState>,
    user: AuthUser,
    Path(approval_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT * FROM approval_requests WHERE id = ?")
        .bind(&approval_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Ownership: the approval must belong to a run of one of the user's tasks
    // (or be a system-level request visible to admins).
    let task_run_id: Option<String> = row.get("task_run_id");
    if let Some(run_id) = &task_run_id {
        let task = sqlx::query("SELECT t.user_id FROM task_runs r JOIN tasks t ON t.id = r.task_id WHERE r.id = ?")
            .bind(run_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let owner: String = task.get("user_id");
        if owner != user.id {
            return Err(ApiError::NotFound);
        }
    } else if !user.is_admin {
        return Err(ApiError::NotFound);
    }

    let operation_json: String = row.get("operation_json");
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "task_run_id": task_run_id,
        "skill_tool_id": row.get::<Option<String>, _>("skill_tool_id"),
        "operation": serde_json::from_str::<serde_json::Value>(&operation_json).unwrap_or(json!({})),
        "status": row.get::<String, _>("status"),
        "expires_at": row.get::<String, _>("expires_at"),
        "created_at": row.get::<String, _>("created_at"),
    })))
}

#[derive(Deserialize)]
pub struct DecideRequest {
    /// The operation the user believes they are approving. Verified against
    /// the stored hash; a mismatch invalidates the approval.
    #[serde(default)]
    pub operation: Option<serde_json::Value>,
    #[serde(default)]
    pub comment: Option<String>,
}

pub async fn approve(
    State(state): State<AppState>,
    user: AuthUser,
    Path(approval_id): Path<String>,
    Json(req): Json<DecideRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ApprovalService::new(&state)
        .decide(&approval_id, &user.id, "approved", req.comment.as_deref(), req.operation.as_ref())
        .await?;
    Ok(Json(json!({ "id": approval_id, "status": "approved" })))
}

pub async fn reject(
    State(state): State<AppState>,
    user: AuthUser,
    Path(approval_id): Path<String>,
    Json(req): Json<DecideRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ApprovalService::new(&state)
        .decide(&approval_id, &user.id, "rejected", req.comment.as_deref(), req.operation.as_ref())
        .await?;
    Ok(Json(json!({ "id": approval_id, "status": "rejected" })))
}

pub async fn cancel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(approval_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    // Only the owner of the related task may cancel.
    let row = sqlx::query("SELECT task_run_id FROM approval_requests WHERE id = ?")
        .bind(&approval_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(run_id) = row.get::<Option<String>, _>("task_run_id") {
        let task = sqlx::query("SELECT t.user_id FROM task_runs r JOIN tasks t ON t.id = r.task_id WHERE r.id = ?")
            .bind(&run_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let owner: String = task.get("user_id");
        if owner != user.id {
            return Err(ApiError::NotFound);
        }
    } else if !user.is_admin {
        return Err(ApiError::NotFound);
    }
    ApprovalService::new(&state).cancel(&approval_id).await?;
    Ok(Json(json!({ "id": approval_id, "status": "cancelled" })))
}