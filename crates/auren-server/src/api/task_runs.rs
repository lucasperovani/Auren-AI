//! Task runs API (spec section 3.7).

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::{auth_user::AuthUser, pagination, tasks::row_to_run},
    error::{ApiError, ApiResult},
    services::task::TaskService,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{run_id}", get(get_run))
        .route("/{run_id}/pause", post(pause_run))
        .route("/{run_id}/resume", post(resume_run))
        .route("/{run_id}/cancel", post(cancel_run))
        .route("/{run_id}/steps", get(list_steps))
        .route("/{run_id}/events", get(list_events))
}

/// Resolve the run and verify the caller owns its task.
async fn require_owned_run(state: &AppState, user: &AuthUser, run_id: &str) -> ApiResult<String> {
    let row = sqlx::query("SELECT task_id FROM task_runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let task_id: String = row.get("task_id");
    TaskService::new(state).require_owned(&user.id, &task_id).await?;
    Ok(task_id)
}

pub async fn get_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let row = sqlx::query("SELECT * FROM task_runs WHERE id = ?").bind(&run_id).fetch_one(&state.db).await?;
    let run = row_to_run(&row);
    Ok(Json(serde_json::to_value(&run).unwrap_or(json!({}))))
}

/// Pause an execution when safe: only queued/pending runs can be paused.
pub async fn pause_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let result = sqlx::query("UPDATE task_runs SET status = 'paused' WHERE id = ? AND status IN ('pending', 'queued')")
        .bind(&run_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict("run cannot be paused in its current state".into()));
    }
    Ok(Json(json!({ "status": "paused" })))
}

pub async fn resume_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let result = sqlx::query("UPDATE task_runs SET status = 'queued' WHERE id = ? AND status = 'paused'")
        .bind(&run_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict("run is not paused".into()));
    }
    Ok(Json(json!({ "status": "queued" })))
}

/// Request cancellation. Cooperative: running steps finish their current
/// external operation; uncertain outcomes are exposed, not hidden.
pub async fn cancel_run(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE queued_jobs SET status = 'cancelled' WHERE task_run_id = ? AND status IN ('pending', 'leased')")
        .bind(&run_id)
        .execute(&state.db)
        .await?;
    let result = sqlx::query(
        "UPDATE task_runs SET status = 'cancelled', finished_at = ?
         WHERE id = ? AND status IN ('pending', 'queued', 'paused')",
    )
    .bind(&now)
    .bind(&run_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        // A running run is marked cancelled at the next safe point; expose
        // the current state rather than a false success.
        let row = sqlx::query("SELECT status FROM task_runs WHERE id = ?").bind(&run_id).fetch_one(&state.db).await?;
        let status: String = row.get("status");
        return Ok(Json(json!({ "status": status, "cancellation": "requested" })));
    }
    Ok(Json(json!({ "status": "cancelled" })))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

pub async fn list_steps(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let cursor_index: i64 = cursor.and_then(|c| c.parse().ok()).unwrap_or(-1);
    let rows = sqlx::query(
        "SELECT * FROM task_steps WHERE task_run_id = ? AND step_index > ? ORDER BY step_index LIMIT ?",
    )
    .bind(&run_id)
    .bind(cursor_index)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;
    let has_more = rows.len() as i64 > limit;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .take(limit as usize)
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "step_index": r.get::<i64, _>("step_index"),
                "status": r.get::<String, _>("status"),
                "input": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>("input_json")).unwrap_or(json!({})),
                "output": r.get::<Option<String>, _>("output_json").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "error_message": r.get::<Option<String>, _>("error_message"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();
    let next_cursor = if has_more {
        items.last().and_then(|i| i["step_index"].as_i64()).map(|i| pagination::encode_cursor(&i.to_string()))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

/// Persisted execution events for the run (also the reconnection source for
/// WebSocket consumers, spec section 4).
pub async fn list_events(
    State(state): State<AppState>,
    user: AuthUser,
    Path(run_id): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_run(&state, &user, &run_id).await?;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM audit_events WHERE task_run_id = ? AND id > ? ORDER BY id LIMIT ?")
                .bind(&run_id).bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM audit_events WHERE task_run_id = ? ORDER BY id LIMIT ?")
                .bind(&run_id).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
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