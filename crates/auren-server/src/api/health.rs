use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use sqlx::Row;

use crate::{error::{ApiError, ApiResult}, state::AppState};

#[derive(Serialize)]
pub struct HealthStatus {
    pub status: &'static str,
}

/// Liveness: the process is running. No sensitive information is exposed.
pub async fn liveness() -> Json<HealthStatus> {
    Json(HealthStatus { status: "ok" })
}

/// Readiness: the database is reachable and migrations have been applied.
pub async fn readiness(State(state): State<AppState>) -> ApiResult<Json<HealthStatus>> {
    let ok = sqlx::query("SELECT 1")
        .fetch_optional(&state.db)
        .await
        .is_ok();
    if !ok {
        return Err(ApiError::Internal(anyhow::anyhow!("database unreachable")));
    }
    Ok(Json(HealthStatus { status: "ready" }))
}

/// Whether initial administrator setup is required (no users exist yet).
pub async fn setup_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT COUNT(*) AS count FROM users")
        .fetch_one(&state.db)
        .await?;
    let count: i64 = row.get("count");
    Ok(Json(serde_json::json!({ "setup_required": count == 0 })))
}

#[derive(serde::Deserialize)]
pub struct SetupRequest {
    pub email: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub is_admin: bool,
    pub created_at: String,
}

/// Create the initial administrator. Only allowed while no user exists.
pub async fn setup(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> ApiResult<(StatusCode, Json<UserResponse>)> {
    validate_credentials(&req.email, &req.password)?;

    let existing = sqlx::query("SELECT COUNT(*) AS count FROM users")
        .fetch_one(&state.db)
        .await?
        .get::<i64, _>("count");
    if existing > 0 {
        return Err(ApiError::Conflict("setup already completed".to_string()));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let hash = crate::auth::hash_password(&req.password)?;

    sqlx::query(
        "INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at)
         VALUES (?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(req.email.trim().to_lowercase())
    .bind(&req.display_name)
    .bind(hash)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(UserResponse {
            id,
            email: req.email.trim().to_lowercase(),
            display_name: req.display_name,
            is_admin: true,
            created_at: now,
        }),
    ))
}

fn validate_credentials(email: &str, password: &str) -> ApiResult<()> {
    if !email.contains('@') || email.len() < 3 {
        return Err(ApiError::Unprocessable("invalid email".to_string()));
    }
    if password.len() < 8 {
        return Err(ApiError::Unprocessable(
            "password must be at least 8 characters".to_string(),
        ));
    }
    Ok(())
}