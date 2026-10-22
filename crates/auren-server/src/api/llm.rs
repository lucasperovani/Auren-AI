//! LLM providers API (spec section 3.3). Stored API keys are never returned.

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
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/providers", get(list_providers).post(create_provider))
        .route("/providers/{provider_id}", get(get_provider).patch(update_provider).delete(delete_provider))
        .route("/providers/{provider_id}/test", post(test_provider))
        .route("/providers/{provider_id}/models", get(list_models).post(add_model))
}

#[derive(Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

#[derive(Serialize)]
pub struct ProviderResponse {
    pub id: String,
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    pub status: String,
    pub has_api_key: bool,
    pub created_at: String,
}

fn row_to_provider(r: &sqlx::sqlite::SqliteRow) -> ProviderResponse {
    let has_key: Option<Vec<u8>> = r.get("encrypted_api_key");
    ProviderResponse {
        id: r.get("id"),
        name: r.get("name"),
        provider_type: r.get("provider_type"),
        base_url: r.get("base_url"),
        status: r.get("status"),
        has_api_key: has_key.is_some(),
        created_at: r.get("created_at"),
    }
}

/// Validate the provider base URL: must be http(s) and must not target
/// internal/loopback destinations (spec 3.3: prevent SSRF to internal hosts).
fn validate_base_url(url: &str) -> ApiResult<()> {
    let parsed = url::Url::parse(url)
        .map_err(|_| ApiError::Unprocessable("invalid base_url".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::Unprocessable("base_url must be http(s)".into()));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host.is_empty()
        || host == "localhost"
        || host == "metadata.google.internal"
        || host.ends_with(".internal")
        || host.ends_with(".local")
    {
        return Err(ApiError::Unprocessable("base_url must not target internal hosts".into()));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let internal = match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified() || v4.is_private(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
        if internal {
            return Err(ApiError::Unprocessable("base_url must not target internal addresses".into()));
        }
    }
    Ok(())
}

pub async fn list_providers(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query("SELECT * FROM llm_providers WHERE user_id = ? ORDER BY created_at")
        .bind(&user.id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<ProviderResponse> = rows.iter().map(row_to_provider).collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

pub async fn create_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProviderRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<ProviderResponse>)> {
    if req.name.trim().is_empty() {
        return Err(ApiError::Unprocessable("name is required".into()));
    }
    if !matches!(
        req.provider_type.as_str(),
        "openai_compatible" | "anthropic" | "ollama" | "custom"
    ) {
        return Err(ApiError::Unprocessable("invalid provider_type".into()));
    }
    validate_base_url(&req.base_url)?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // API keys are stored encrypted at rest (spec section 14); the
    // plaintext is never persisted or returned.
    let encrypted = match &req.api_key {
        Some(key) => {
            let (ct, nonce) = crate::services::credentials::encrypt_credential(key, &state.config.credential_encryption_key)?;
            Some((ct, nonce))
        }
        None => None,
    };
    sqlx::query(
        "INSERT INTO llm_providers (id, user_id, name, provider_type, base_url, encrypted_api_key, key_nonce, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(req.name.trim())
    .bind(&req.provider_type)
    .bind(&req.base_url)
    .bind(encrypted.as_ref().map(|(ct, _)| ct.clone()))
    .bind(encrypted.as_ref().map(|(_, n)| n.clone()))
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    let row = sqlx::query("SELECT * FROM llm_providers WHERE id = ?").bind(&id).fetch_one(&state.db).await?;
    Ok((axum::http::StatusCode::CREATED, Json(row_to_provider(&row))))
}

async fn require_owned_provider(state: &AppState, user: &AuthUser, provider_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM llm_providers WHERE id = ?")
        .bind(provider_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
) -> ApiResult<Json<ProviderResponse>> {
    require_owned_provider(&state, &user, &provider_id).await?;
    let row = sqlx::query("SELECT * FROM llm_providers WHERE id = ?")
        .bind(&provider_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_provider(&row)))
}

#[derive(Deserialize)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub status: Option<String>,
}

pub async fn update_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
    Json(req): Json<UpdateProviderRequest>,
) -> ApiResult<Json<ProviderResponse>> {
    require_owned_provider(&state, &user, &provider_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(n) = &req.name {
        sqlx::query("UPDATE llm_providers SET name = ?, updated_at = ? WHERE id = ?")
            .bind(n.trim()).bind(&now).bind(&provider_id).execute(&state.db).await?;
    }
    if let Some(u) = &req.base_url {
        validate_base_url(u)?;
        sqlx::query("UPDATE llm_providers SET base_url = ?, updated_at = ? WHERE id = ?")
            .bind(u).bind(&now).bind(&provider_id).execute(&state.db).await?;
    }
    if let Some(k) = &req.api_key {
        let (ct, nonce) = crate::services::credentials::encrypt_credential(k, &state.config.credential_encryption_key)?;
        sqlx::query("UPDATE llm_providers SET encrypted_api_key = ?, key_nonce = ?, updated_at = ? WHERE id = ?")
            .bind(ct).bind(nonce).bind(&now).bind(&provider_id).execute(&state.db).await?;
    }
    if let Some(s) = &req.status {
        if !matches!(s.as_str(), "active" | "disabled") {
            return Err(ApiError::Unprocessable("invalid status".into()));
        }
        sqlx::query("UPDATE llm_providers SET status = ?, updated_at = ? WHERE id = ?")
            .bind(s).bind(&now).bind(&provider_id).execute(&state.db).await?;
    }
    let row = sqlx::query("SELECT * FROM llm_providers WHERE id = ?")
        .bind(&provider_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_provider(&row)))
}

/// Remove a provider. Blocked while agents still depend on it (spec 3.3:
/// handle the dependency explicitly rather than leaving tasks broken).
pub async fn delete_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    require_owned_provider(&state, &user, &provider_id).await?;
    let dependents: i64 = sqlx::query("SELECT COUNT(*) AS c FROM agents WHERE llm_provider_id = ? AND status != 'archived'")
        .bind(&provider_id)
        .fetch_one(&state.db)
        .await?
        .get("c");
    if dependents > 0 {
        return Err(ApiError::Conflict(format!(
            "{dependents} active agent(s) still use this provider; reassign them first"
        )));
    }
    sqlx::query("DELETE FROM llm_providers WHERE id = ?")
        .bind(&provider_id)
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Test provider connectivity. Performs a lightweight request against the
/// configured base URL; never returns the stored key or provider internals.
pub async fn test_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_provider(&state, &user, &provider_id).await?;
    let row = sqlx::query("SELECT base_url FROM llm_providers WHERE id = ?")
        .bind(&provider_id)
        .fetch_one(&state.db)
        .await?;
    let base_url: String = row.get("base_url");

    let client = reqwest::Client::new()
        .get(&base_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;
    match client {
        Ok(resp) => Ok(Json(json!({
            "reachable": true,
            "http_status": resp.status().as_u16(),
        }))),
        Err(_) => Ok(Json(json!({ "reachable": false }))),
    }
}

#[derive(Deserialize)]
pub struct AddModelRequest {
    pub model_name: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
}

/// Register a model with the provider. Capabilities are stored explicitly
/// (spec section 2.2: never assume tool calling or structured output).
pub async fn add_model(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
    Json(req): Json<AddModelRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    require_owned_provider(&state, &user, &provider_id).await?;
    if req.model_name.trim().is_empty() {
        return Err(ApiError::Unprocessable("model_name is required".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let capabilities = if req.capabilities.is_null() || req.capabilities.as_object().is_none() {
        serde_json::json!({})
    } else {
        req.capabilities
    };
    sqlx::query(
        "INSERT INTO llm_models (id, provider_id, model_name, capabilities_json, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&provider_id)
    .bind(req.model_name.trim())
    .bind(capabilities.to_string())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.db)
    .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "model_name": req.model_name.trim() })),
    ))
}

pub async fn list_models(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_provider(&state, &user, &provider_id).await?;
    let rows = sqlx::query("SELECT * FROM llm_models WHERE provider_id = ? ORDER BY model_name")
        .bind(&provider_id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let caps: String = r.get("capabilities_json");
            json!({
                "id": r.get::<String, _>("id"),
                "model_name": r.get::<String, _>("model_name"),
                "capabilities": serde_json::from_str::<serde_json::Value>(&caps).unwrap_or(json!({})),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}