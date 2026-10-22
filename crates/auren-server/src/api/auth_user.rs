//! Typed authenticated-user extractor for protected endpoints.
//!
//! Endpoints accept `Authorization: Bearer <session_token>` (or the raw
//! token in the header for first-party clients). Session validation is the
//! single shared code path (spec section 9: no duplicated security logic).

use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use chrono::Utc;
use sqlx::Row;

use crate::{
    auth,
    error::{ApiError, ApiResult},
    state::AppState,
};

/// The authenticated user resolved from the request's session token.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub is_admin: bool,
}

impl AuthUser {
    /// Validate a session token against the database.
    pub async fn from_token(state: &AppState, token: &str) -> ApiResult<Self> {
        let token_hash = auth::hash_session_token(token, &state.config.session_secret);
        let row = sqlx::query(
            "SELECT u.id, u.email, u.display_name, u.is_admin, s.expires_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::Unauthorized)?;

        let expires_at: String = row.get("expires_at");
        let expires = chrono::DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| ApiError::Internal(anyhow::anyhow!("invalid session expiry")))?;
        if expires < Utc::now() {
            return Err(ApiError::Unauthorized);
        }

        Ok(Self {
            id: row.get("id"),
            email: row.get("email"),
            display_name: row.get("display_name"),
            is_admin: row.get::<i64, _>("is_admin") == 1,
        })
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .unwrap_or(header);

        Self::from_token(state, token).await
    }
}

/// Validate a session token and return the authenticated user (legacy JSON
/// form used by the auth endpoints that take the token in the body).
pub async fn authenticate(
    state: &AppState,
    token: &str,
) -> ApiResult<serde_json::Value> {
    let user = AuthUser::from_token(state, token).await?;
    Ok(serde_json::json!({
        "id": user.id,
        "email": user.email,
        "display_name": user.display_name,
        "is_admin": user.is_admin,
    }))
}