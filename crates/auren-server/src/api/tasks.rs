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
        .route("/{task_id}/stage", post(move_task_stage))
        .route("/{task_id}/assign", post(assign_task))
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
    /// Filter the board by project. `"none"` selects the default board.
    pub project_id: Option<String>,
}

#[derive(Serialize)]
pub struct TaskResponse {
    pub id: String,
    pub agent_id: String,
    pub conversation_id: Option<String>,
    pub title: String,
    pub objective: String,
    pub status: String,
    /// Kanban column: `analysis`, `todo`, `in_progress`, or `done`.
    pub stage: String,
    /// Owning project, or `null` for the default board.
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub schedule: Option<serde_json::Value>,
    /// Raw cron expression when the task is scheduled. Clients render a
    /// human description; the expression itself is never shown to users.
    pub schedule_cron: Option<String>,
    /// Next scheduled fire time (RFC 3339), when a schedule is active.
    pub next_run_at: Option<String>,
    /// Status and finish time of the most recent run, for list summaries.
    pub last_run_status: Option<String>,
    pub last_run_at: Option<String>,
    pub timezone: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Task columns plus the joined schedule and latest run, so list and detail
/// endpoints agree on the shape consumed by `row_to_task`.
const TASK_SELECT: &str = "SELECT t.*, j.next_run_at AS next_run_at, \
     p.name AS project_name, \
     r.status AS last_run_status, r.created_at AS last_run_at \
     FROM tasks t \
     LEFT JOIN scheduled_jobs j ON j.task_id = t.id AND j.status = 'active' \
     LEFT JOIN projects p ON p.id = t.project_id \
     LEFT JOIN task_runs r ON r.id = (\
         SELECT id FROM task_runs WHERE task_id = t.id ORDER BY created_at DESC LIMIT 1\
     )";

fn row_to_task(r: &sqlx::sqlite::SqliteRow) -> TaskResponse {
    let schedule_json: Option<String> = r.get("schedule_json");
    let schedule: Option<serde_json::Value> =
        schedule_json.and_then(|s| serde_json::from_str(&s).ok());
    let schedule_cron = schedule
        .as_ref()
        .and_then(|s| s.get("cron"))
        .and_then(|c| c.as_str())
        .map(|c| c.to_string());
    TaskResponse {
        id: r.get("id"),
        agent_id: r.get("agent_id"),
        conversation_id: r.get("conversation_id"),
        title: r.get("title"),
        objective: r.get("objective"),
        status: r.get("status"),
        stage: r.try_get("stage").unwrap_or_else(|_| "todo".to_string()),
        project_id: r.try_get("project_id").ok().flatten(),
        project_name: r.try_get("project_name").ok().flatten(),
        schedule,
        schedule_cron,
        next_run_at: r.try_get("next_run_at").ok().flatten(),
        last_run_status: r.try_get("last_run_status").ok().flatten(),
        last_run_at: r.try_get("last_run_at").ok().flatten(),
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

    let mut sql = format!("{TASK_SELECT} WHERE t.user_id = ?");
    if params.status.is_some() {
        sql.push_str(" AND t.status = ?");
    }
    if params.agent_id.is_some() {
        sql.push_str(" AND t.agent_id = ?");
    }
    // Project scoping: an explicit id narrows the board, `none` selects only
    // tasks without a project, and no value returns everything.
    match params.project_id.as_deref() {
        Some("none") => sql.push_str(" AND t.project_id IS NULL"),
        Some(_) => sql.push_str(" AND t.project_id = ?"),
        None => {}
    }
    sql.push_str(" AND t.id > ? ORDER BY t.id LIMIT ?");

    let mut query = sqlx::query(&sql).bind(&user.id);
    if let Some(s) = &params.status {
        query = query.bind(s);
    }
    if let Some(a) = &params.agent_id {
        query = query.bind(a);
    }
    if let Some(p) = params.project_id.as_deref().filter(|p| *p != "none") {
        query = query.bind(p);
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
    /// Owning project. Omit for the default board.
    #[serde(default)]
    pub project_id: Option<String>,
}

fn default_timezone() -> String {
    "UTC".into()
}

pub async fn create_task(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateTaskRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<TaskResponse>)> {
    // A project must belong to the caller before a task can join it.
    if let Some(project_id) = req.project_id.as_deref() {
        crate::services::projects::ProjectService::new(&state)
            .require_owned(&user.id, project_id)
            .await?;
    }
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
            req.project_id.as_deref(),
        )
        .await?;
    let row = sqlx::query(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    Ok((axum::http::StatusCode::CREATED, Json(row_to_task(&row))))
}

pub async fn get_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<TaskResponse>> {
    TaskService::new(&state).require_owned(&user.id, &task_id).await?;
    let row = sqlx::query(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(&task_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_task(&row)))
}

#[derive(Deserialize)]
pub struct UpdateTaskRequest {
    pub title: Option<String>,
    pub objective: Option<String>,
    /// Kanban column. Sent by the board when a card is dragged.
    #[serde(default)]
    pub stage: Option<String>,
    /// `None` = field absent (leave unchanged); `Some(None)` = explicit null
    /// (remove the schedule); `Some(Some(v))` = replace the schedule.
    #[serde(default, deserialize_with = "double_option")]
    pub schedule: Option<Option<serde_json::Value>>,
}

/// Distinguish a missing field from an explicit `null`. Plain `Option`
/// collapses both cases, which would make "remove the schedule" impossible.
fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<serde_json::Value>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<serde_json::Value>::deserialize(deserializer).map(Some)
}

pub async fn update_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<UpdateTaskRequest>,
) -> ApiResult<Json<TaskResponse>> {
    let svc = TaskService::new(&state);
    match req.schedule {
        Some(Some(schedule)) => {
            svc.update(&user.id, &task_id, req.title.as_deref(), req.objective.as_deref(), Some(&schedule))
                .await?;
        }
        Some(None) => {
            svc.clear_schedule(&user.id, &task_id).await?;
            svc.update(&user.id, &task_id, req.title.as_deref(), req.objective.as_deref(), None)
                .await?;
        }
        None => {
            svc.update(&user.id, &task_id, req.title.as_deref(), req.objective.as_deref(), None)
                .await?;
        }
    }
    if let Some(stage) = req.stage.as_deref() {
        svc.set_stage(&user.id, &task_id, stage).await?;
    }
    let row = sqlx::query(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(&task_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_task(&row)))
}

#[derive(Deserialize)]
pub struct MoveStageRequest {
    pub stage: String,
}

/// Move a task to another board column. This never changes the execution
/// status, so dragging a card cannot silently pause a schedule.
pub async fn move_task_stage(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<MoveStageRequest>,
) -> ApiResult<Json<TaskResponse>> {
    TaskService::new(&state)
        .set_stage(&user.id, &task_id, &req.stage)
        .await?;
    let row = sqlx::query(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(&task_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_task(&row)))
}

#[derive(Deserialize)]
pub struct AssignTaskRequest {
    /// Optional project override when assigning from a project board.
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Assign a task to the agent for immediate execution: this is what the
/// assignment dialog confirms. It moves the card to In Progress and queues a
/// run, which is the only path where a stage change starts work.
pub async fn assign_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(_req): Json<AssignTaskRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let collab = crate::services::task_collab::TaskCollabService::new(&state);
    let svc = TaskService::new(&state);
    svc.require_owned(&user.id, &task_id).await?;

    // Refuse to start work that depends on unfinished cards: the run would
    // fail or, worse, produce a result based on missing input.
    let blockers = collab.open_blockers(&task_id).await?;
    if !blockers.is_empty() {
        return Err(ApiError::Conflict(format!(
            "this task is blocked by unfinished work: {}",
            blockers.join(", ")
        )));
    }

    svc.set_stage(&user.id, &task_id, "in_progress").await?;
    let run_id = svc.request_run(&user.id, &task_id).await?;
    let row = sqlx::query(&format!("{TASK_SELECT} WHERE t.id = ?"))
        .bind(&task_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({
        "task": row_to_task(&row),
        "task_run_id": run_id,
        "status": "assigned",
    })))
}

/// Permanently delete a task: cancels active work, removes the schedule,
/// and deletes the record (runs/steps cascade).
pub async fn delete_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    TaskService::new(&state).delete(&user.id, &task_id).await?;
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
    let row = sqlx::query(
        "SELECT j.schedule_json, t.timezone FROM scheduled_jobs j
         JOIN tasks t ON t.id = j.task_id WHERE j.task_id = ?",
    )
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