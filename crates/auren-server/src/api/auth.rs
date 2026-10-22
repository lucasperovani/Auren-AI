use axum::{extract::State, routing::{get, post}, Json, Router};
use chrono::{Duration, Utc};
use serde::Deserialize;
use sqlx::Row;

use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/setup-status", get(super::health::setup_status))
        .route("/setup", post(super::health::setup))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", post(me))
        .route("/change-password", post(change_password))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Authenticate and issue a session. The plaintext token is returned once and
/// only its hash is stored (spec 2.2). Failed attempts are rate limited per
/// email (spec section 15).
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let identity = req.email.trim().to_lowercase();
    if state.rate_limiter.is_locked(&identity) {
        return Err(ApiError::TooManyRequests);
    }

    let row = sqlx::query("SELECT id, password_hash FROM users WHERE email = ?")
        .bind(&identity)
        .fetch_optional(&state.db)
        .await?;

    let (user_id, password_hash) = match row {
        Some(r) => (r.get::<String, _>("id"), r.get::<String, _>("password_hash")),
        // Constant-ish response: do not reveal whether the email exists.
        None => {
            state.rate_limiter.record_failure(&identity);
            return Err(ApiError::Unauthorized);
        }
    };

    if !auth::verify_password(&req.password, &password_hash)? {
        state.rate_limiter.record_failure(&identity);
        return Err(ApiError::Unauthorized);
    }
    state.rate_limiter.clear(&identity);

    let token = auth::generate_session_token();
    let token_hash = auth::hash_session_token(&token, &state.config.session_secret);
    let now = Utc::now();
    let expires = now + Duration::days(30);

    sqlx::query("INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&user_id)
        .bind(token_hash)
        .bind(expires.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({
        "session_token": token,
        "expires_at": expires.to_rfc3339(),
    })))
}

#[derive(Deserialize)]
pub struct TokenRequest {
    pub session_token: String,
}

/// Revoke the current session.
pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<TokenRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let token_hash = auth::hash_session_token(&req.session_token, &state.config.session_secret);
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Retrieve the current authenticated user.
pub async fn me(
    State(state): State<AppState>,
    Json(req): Json<TokenRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = authenticate(&state, &req.session_token).await?;
    Ok(Json(user))
}

/// Change the authenticated user's password and revoke other sessions.
pub async fn change_password(
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = authenticate(&state, &req.session_token).await?;
    let user_id = user["id"].as_str().unwrap_or_default().to_string();

    let row = sqlx::query("SELECT password_hash FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_one(&state.db)
        .await?;
    let current_hash: String = row.get("password_hash");

    if !auth::verify_password(&req.current_password, &current_hash)? {
        return Err(ApiError::Forbidden("current password is incorrect".to_string()));
    }
    if req.new_password.len() < 8 {
        return Err(ApiError::Unprocessable(
            "password must be at least 8 characters".to_string(),
        ));
    }

    let new_hash = auth::hash_password(&req.new_password)?;
    let now = Utc::now().to_rfc3339();
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(new_hash)
        .bind(&now)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    // Revoke all other sessions after a password change.
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Validate a session token and return the authenticated user.
pub async fn authenticate(
    state: &AppState,
    token: &str,
) -> ApiResult<serde_json::Value> {
    let token_hash = auth::hash_session_token(token, &state.config.session_secret);
    let row = sqlx::query(
        "SELECT u.id, u.email, u.display_name, u.is_admin, s.expires_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = ?",
    )
    .bind(token_hash)
    .fetch_optional(&state.db)
    .await?;

    let row = row.ok_or(ApiError::Unauthorized)?;
    let expires_at: String = row.get("expires_at");
    let expires = chrono::DateTime::parse_from_rfc3339(&expires_at)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("invalid session expiry")))?;
    if expires < Utc::now() {
        return Err(ApiError::Unauthorized);
    }

    Ok(serde_json::json!({
        "id": row.get::<String, _>("id"),
        "email": row.get::<String, _>("email"),
        "display_name": row.get::<String, _>("display_name"),
        "is_admin": row.get::<i64, _>("is_admin") == 1,
    }))
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub session_token: String,
    pub current_password: String,
    pub new_password: String,
}