//! Natural-language task planning API (spec section 3.8).
//!
//! A plan is a proposal, not an active task: it must pass validation and be
//! confirmed before a persistent task and schedule are created
//! transactionally.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    services::task::TaskService,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_plan))
        .route("/{plan_id}", get(get_plan).patch(refine_plan))
        .route("/{plan_id}/clarifications", post(submit_clarifications))
        .route("/{plan_id}/confirm", post(confirm_plan))
        .route("/{plan_id}/cancel", post(cancel_plan))
}

/// Plans are persisted in `app_settings` under a namespaced key until
/// confirmed; the confirmed task lives in `tasks`.
const KEY_PREFIX: &str = "task_plan:";

#[derive(Serialize, Deserialize, Clone)]
pub struct TaskPlan {
    pub plan_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub request: String,
    pub title: String,
    pub objective: String,
    pub schedule: Option<serde_json::Value>,
    pub timezone: String,
    /// Missing information the user must supply before confirmation.
    pub clarifications: Vec<Clarification>,
    pub required_skills: Vec<String>,
    pub required_integrations: Vec<String>,
    pub approval_requirements: Vec<String>,
    pub status: String, // proposed | clarified | confirmed | cancelled
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Clarification {
    pub question: String,
    pub answer: Option<String>,
}

async fn load_plan(state: &AppState, plan_id: &str) -> ApiResult<TaskPlan> {
    let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = ?")
        .bind(format!("{KEY_PREFIX}{plan_id}"))
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let json_str: String = row.get("value_json");
    let plan: TaskPlan = serde_json::from_str(&json_str)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("corrupt plan payload")))?;
    Ok(plan)
}

async fn save_plan(state: &AppState, plan: &TaskPlan) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(format!("{KEY_PREFIX}{}", plan.plan_id))
    .bind(serde_json::to_string(plan).unwrap_or_default())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Interpret a natural-language request into a proposed plan. The initial
/// heuristic planner extracts the objective verbatim and asks for the
/// details it cannot infer; the LLM-backed planner replaces this once a
/// provider is configured.
pub async fn create_plan(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreatePlanRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<TaskPlan>)> {
    if req.request.trim().is_empty() {
        return Err(ApiError::Unprocessable("request is required".into()));
    }
    // The agent must belong to the user.
    let owner: Option<String> = sqlx::query("SELECT user_id FROM agents WHERE id = ?")
        .bind(&req.agent_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("user_id"));
    if owner.as_deref() != Some(user.id.as_str()) {
        return Err(ApiError::NotFound);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let plan = TaskPlan {
        plan_id: uuid::Uuid::new_v4().to_string(),
        user_id: user.id,
        agent_id: req.agent_id,
        request: req.request.trim().to_string(),
        title: req.request.trim().chars().take(80).collect(),
        objective: req.request.trim().to_string(),
        schedule: req.schedule,
        timezone: req.timezone.unwrap_or_else(|| "UTC".into()),
        // Essential details the planner cannot infer are asked, not invented.
        clarifications: vec![Clarification {
            question: "When should this task run, and how often?".into(),
            answer: None,
        }],
        required_skills: vec![],
        required_integrations: vec![],
        approval_requirements: vec![],
        status: "proposed".into(),
        created_at: now.clone(),
        updated_at: now,
    };
    save_plan(&state, &plan).await?;
    Ok((axum::http::StatusCode::CREATED, Json(plan)))
}

#[derive(Deserialize)]
pub struct CreatePlanRequest {
    pub agent_id: String,
    pub request: String,
    pub schedule: Option<serde_json::Value>,
    pub timezone: Option<String>,
}

pub async fn get_plan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<String>,
) -> ApiResult<Json<TaskPlan>> {
    let plan = load_plan(&state, &plan_id).await?;
    if plan.user_id != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(Json(plan))
}

#[derive(Deserialize)]
pub struct RefinePlanRequest {
    pub title: Option<String>,
    pub objective: Option<String>,
    pub schedule: Option<serde_json::Value>,
}

pub async fn refine_plan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<String>,
    Json(req): Json<RefinePlanRequest>,
) -> ApiResult<Json<TaskPlan>> {
    let mut plan = load_plan(&state, &plan_id).await?;
    if plan.user_id != user.id {
        return Err(ApiError::NotFound);
    }
    if plan.status == "confirmed" || plan.status == "cancelled" {
        return Err(ApiError::Conflict("plan is closed".into()));
    }
    if let Some(t) = req.title {
        plan.title = t;
    }
    if let Some(o) = req.objective {
        plan.objective = o;
    }
    if let Some(s) = req.schedule {
        plan.schedule = Some(s);
    }
    plan.updated_at = chrono::Utc::now().to_rfc3339();
    save_plan(&state, &plan).await?;
    Ok(Json(plan))
}

#[derive(Deserialize)]
pub struct ClarificationsRequest {
    pub answers: Vec<Clarification>,
}

pub async fn submit_clarifications(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<String>,
    Json(req): Json<ClarificationsRequest>,
) -> ApiResult<Json<TaskPlan>> {
    let mut plan = load_plan(&state, &plan_id).await?;
    if plan.user_id != user.id {
        return Err(ApiError::NotFound);
    }
    if plan.status == "confirmed" || plan.status == "cancelled" {
        return Err(ApiError::Conflict("plan is closed".into()));
    }
    // Merge answers into the pending questions by position.
    for (i, answer) in req.answers.into_iter().enumerate() {
        if let Some(existing) = plan.clarifications.get_mut(i) {
            existing.answer = answer.answer;
        }
    }
    plan.status = "clarified".into();
    plan.updated_at = chrono::Utc::now().to_rfc3339();
    save_plan(&state, &plan).await?;
    Ok(Json(plan))
}

/// Validate and create the persistent task. Confirmation fails while
/// required clarifications are unanswered (spec 3.8: ask, don't invent).
pub async fn confirm_plan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let plan = load_plan(&state, &plan_id).await?;
    if plan.user_id != user.id {
        return Err(ApiError::NotFound);
    }
    if plan.status == "confirmed" {
        return Err(ApiError::Conflict("plan already confirmed".into()));
    }
    if plan.status == "cancelled" {
        return Err(ApiError::Conflict("plan was cancelled".into()));
    }
    let unanswered: Vec<&str> = plan
        .clarifications
        .iter()
        .filter(|c| c.answer.is_none())
        .map(|c| c.question.as_str())
        .collect();
    if !unanswered.is_empty() {
        return Err(ApiError::Unprocessable(format!(
            "plan is missing required information: {}",
            unanswered.join("; ")
        )));
    }

    let svc = TaskService::new(&state);
    let task_id = svc
        .create(
            &plan.user_id,
            &plan.agent_id,
            &plan.title,
            &plan.objective,
            plan.schedule.as_ref(),
            &plan.timezone,
            None,
        )
        .await?;

    let mut confirmed = plan.clone();
    confirmed.status = "confirmed".into();
    confirmed.updated_at = chrono::Utc::now().to_rfc3339();
    save_plan(&state, &confirmed).await?;

    Ok(Json(json!({ "task_id": task_id, "plan_id": plan.plan_id, "status": "confirmed" })))
}

pub async fn cancel_plan(
    State(state): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<String>,
) -> ApiResult<Json<TaskPlan>> {
    let mut plan = load_plan(&state, &plan_id).await?;
    if plan.user_id != user.id {
        return Err(ApiError::NotFound);
    }
    if plan.status == "confirmed" {
        return Err(ApiError::Conflict("confirmed plans cannot be cancelled; cancel the task instead".into()));
    }
    plan.status = "cancelled".into();
    plan.updated_at = chrono::Utc::now().to_rfc3339();
    save_plan(&state, &plan).await?;
    Ok(Json(plan))
}