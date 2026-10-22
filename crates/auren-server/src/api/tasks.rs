//! Tasks API (spec section 3.6). All mutations go through TaskService so the
//! REST API and the conversational agent enforce the same business rules.

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::{
    api::{auth_user::AuthUser, pagination},
    error::{ApiError, ApiResult},
    services::task::TaskService,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_tasks).post(create_task))
        .route("/{task_id}", get(get_task).patch(update_task).delete(delete_task))
        .route("/{task_id}/run", post(run_task))
        .route("/{task_id}/pause", post(pause_task))
        .route("/{task_id}/resume", post(resume_task))
        .route("/{task_id}/cancel", post(cancel_task))
        .route("/{task_id}/runs", get(list_runs))
        .route("/{task_id}/schedule", get(get_schedule).put(put_schedule))
        .route("/{task_id}/steps", get(get_steps))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Serialize)]
pub struct TaskResponse {
    pub id: String,
    pub agent_id: String,
    pub conversation_id: Option<String>,
    pub title: String,
    pub objective: String,
    pub status: String,
    pub schedule: Option<serde_json::Value>,
    pub timezone: String,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_task(r: &sqlx::sqlite::SqliteRow) -> TaskResponse {
    let schedule_json: Option<String> = r.get("schedule_json");
    TaskResponse {
        id: r.get("id"),
        agent_id: r.get("agent_id"),
        conversation_id: r.get("conversation_id"),
        title: r.get("title"),
        objective: r.get("objective"),
        status: r.get("status"),
        schedule: schedule_json.and_then(|s| serde_json::from_str(&s).ok()),
        timezone: r.get("timezone"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn list_tasks(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);

    let mut sql = String::from("SELECT * FROM tasks WHERE user_id = ?");
    if params.status.is_some() {
        sql.push_str(" AND status = ?");
    }
    if params.agent_id.is_some() {
        sql.push_str(" AND agent_id = ?");
    }
    sql.push_str(" AND id > ? ORDER BY id LIMIT ?");

    let mut query = sqlx::query(&sql).bind(&user.id);
    if let Some(s) = &params.status {
        query = query.bind(s);
    }
    if let Some(a) = &params.agent_id {
        query = query.bind(a);
    }
    let cursor_value = cursor.clone().unwrap_or_default();
    query = query.bind(&cursor_value).bind(limit + 1);
    let rows = query.fetch_all(&state.db).await?;

    let has_more = rows.len() as i64 > limit;
    let items: Vec<TaskResponse> = rows.iter().take(limit as usize).map(row_to_task).collect();
    let next_cursor = if has_more {
        items.last().map(|t| pagination::encode_cursor(&t.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

#[derive(Deserialize)]
pub struct CreateTaskRequest {
    pub agent_id: String,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub schedule: Option<serde_json::Value>,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub conversation_id: Option<String>,
}

fn default_timezone() -> String {
    "UTC".into()
}

pub async fn create_task(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateTaskRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<TaskResponse>)> {
    let svc = TaskService::new(&state);
    let id = svc
        .create(
            &user.id,
            &req.agent_id,
            &req.title,
            &req.objective,
            req.schedule.as_ref(),
            &req.timezone,
            req.conversation_id.as_deref(),
        )
        .await?;
    let row = sqlx::query("SELECT * FROM tasks WHERE id = ?").bind(&id).fetch_one(&state.db).await?;
    Ok((axum::http::StatusCode::CREATED, Json(row_to_task(&row))))
}

pub async fn get_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<TaskResponse>> {
    TaskService::new(&state).require_owned(&user.id, &task_id).await?;
    let row = sqlx::query("SELECT * FROM tasks WHERE id = ?").bind(&task_id).fetch_one(&state.db).await?;
    Ok(Json(row_to_task(&row)))
}

#[derive(Deserialize)]
pub struct UpdateTaskRequest {
    pub title: Option<String>,
    pub objective: Option<String>,
    pub schedule: Option<serde_json::Value>,
}

pub async fn update_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<UpdateTaskRequest>,
) -> ApiResult<Json<TaskResponse>> {
    let svc = TaskService::new(&state);
    svc.update(&user.id, &task_id, req.title.as_deref(), req.objective.as_deref(), req.schedule.as_ref())
        .await?;
    let row = sqlx::query("SELECT * FROM tasks WHERE id = ?").bind(&task_id).fetch_one(&state.db).await?;
    Ok(Json(row_to_task(&row)))
}

/// Cancel and archive a task. Active runs are cancelled cooperatively.
pub async fn delete_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let svc = TaskService::new(&state);
    svc.set_status(&user.id, &task_id, "cancelled").await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn run_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let svc = TaskService::new(&state);
    let run_id = svc.request_run(&user.id, &task_id).await?;
    Ok((axum::http::StatusCode::ACCEPTED, Json(json!({ "task_run_id": run_id, "status": "queued" }))))
}

pub async fn pause_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state).set_status(&user.id, &task_id, "paused").await?;
    Ok(Json(json!({ "status": "paused" })))
}

pub async fn resume_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state).set_status(&user.id, &task_id, "active").await?;
    Ok(Json(json!({ "status": "active" })))
}

/// Cancel active work where possible. Cancellation is cooperative: already
/// completed external operations are not reversed (spec 3.7).
pub async fn cancel_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = TaskService::new(&state);
    svc.require_owned(&user.id, &task_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE task_runs SET status = 'cancelled', finished_at = ?
         WHERE task_id = ? AND status IN ('pending', 'queued', 'paused')",
    )
    .bind(&now)
    .bind(&task_id)
    .execute(&state.db)
    .await?;
    svc.set_status(&user.id, &task_id, "cancelled").await?;
    Ok(Json(json!({ "status": "cancelled" })))
}

#[derive(Serialize)]
pub struct RunResponse {
    pub id: String,
    pub task_id: String,
    pub status: String,
    pub trigger: String,
    pub scheduled_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub created_at: String,
}

pub fn row_to_run(r: &sqlx::sqlite::SqliteRow) -> RunResponse {
    let result_json: Option<String> = r.get("result_json");
    RunResponse {
        id: r.get("id"),
        task_id: r.get("task_id"),
        status: r.get("status"),
        trigger: r.get("trigger"),
        scheduled_at: r.get("scheduled_at"),
        started_at: r.get("started_at"),
        finished_at: r.get("finished_at"),
        result: result_json.and_then(|s| serde_json::from_str(&s).ok()),
        error_message: r.get("error_message"),
        created_at: r.get("created_at"),
    }
}

pub async fn list_runs(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state).require_owned(&user.id, &task_id).await?;
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM task_runs WHERE task_id = ? AND id > ? ORDER BY id LIMIT ?")
                .bind(&task_id).bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM task_runs WHERE task_id = ? ORDER BY id LIMIT ?")
                .bind(&task_id).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<RunResponse> = rows.iter().take(limit as usize).map(row_to_run).collect();
    let next_cursor = if has_more {
        items.last().map(|r| pagination::encode_cursor(&r.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

pub async fn get_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state).require_owned(&user.id, &task_id).await?;
    let row = sqlx::query("SELECT schedule_json, timezone FROM scheduled_jobs WHERE task_id = ?")
        .bind(&task_id)
        .fetch_optional(&state.db)
        .await?;
    match row {
        Some(r) => {
            let schedule_json: String = r.get("schedule_json");
            Ok(Json(json!({
                "schedule": serde_json::from_str::<serde_json::Value>(&schedule_json).unwrap_or(json!({})),
                "timezone": r.get::<String, _>("timezone"),
            })))
        }
        None => Ok(Json(json!({ "schedule": null }))),
    }
}

#[derive(Deserialize)]
pub struct PutScheduleRequest {
    pub schedule: serde_json::Value,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

pub async fn put_schedule(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<PutScheduleRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state)
        .update(&user.id, &task_id, None, None, Some(&req.schedule))
        .await?;
    Ok(Json(json!({ "schedule": req.schedule, "timezone": req.timezone })))
}

pub async fn get_steps(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    TaskService::new(&state).require_owned(&user.id, &task_id).await?;
    // Latest run's steps.
    let run = sqlx::query("SELECT id FROM task_runs WHERE task_id = ? ORDER BY created_at DESC LIMIT 1")
        .bind(&task_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let run_id: String = run.get("id");
    let rows = sqlx::query("SELECT * FROM task_steps WHERE task_run_id = ? ORDER BY step_index")
        .bind(&run_id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "task_run_id": r.get::<String, _>("task_run_id"),
                "step_index": r.get::<i64, _>("step_index"),
                "status": r.get::<String, _>("status"),
                "input": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>("input_json")).unwrap_or(json!({})),
                "output": r.get::<Option<String>, _>("output_json").and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "error_message": r.get::<Option<String>, _>("error_message"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}