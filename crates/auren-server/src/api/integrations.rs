//! Integrations API (spec section 3.9). Credential values are never exposed.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_integrations))
        .route("/catalog", get(catalog))
        .route("/oauth/callback", get(oauth_callback))
        .route("/{provider_type}/connect", post(connect))
        .route("/{integration_id}", get(get_integration).delete(delete_integration))
        .route("/{integration_id}/accounts", get(list_accounts))
        .route("/{integration_id}/accounts/{account_id}", get(get_account))
        .route("/{integration_id}/accounts/{account_id}/test", post(test_account))
        .route("/{integration_id}/accounts/{account_id}/reconnect", post(reconnect_account))
        .route("/{integration_id}/accounts/{account_id}/capabilities", get(account_capabilities))
}

/// Available integration provider types (spec 3.9: the capability list must
/// reflect implemented operations, not theoretical provider APIs).
const CATALOG: &[(&str, &[&str])] = &[
    ("gmail", &["search_messages", "read_message", "create_draft", "send_message"]),
    ("gcal", &["list_events", "search_events", "create_event", "update_event", "cancel_event"]),
    ("gphotos", &["list_albums", "search_media", "get_media"]),
    ("instagram", &["get_account_info", "list_media", "publish_media"]),
    ("flights", &["search_itineraries", "get_price", "monitor_price", "prepare_booking", "complete_purchase"]),
    ("github", &["list_files", "read_file", "write_file", "list_issues", "create_issue", "create_pr", "me", "list_repos", "search_repos", "list_prs", "get_pr", "merge_pr", "git_clone", "git_pull", "git_fetch", "git_push"]),
];

pub async fn catalog() -> Json<serde_json::Value> {
    let items: Vec<serde_json::Value> = CATALOG
        .iter()
        .map(|(t, ops)| json!({ "provider_type": t, "operations": ops }))
        .collect();
    Json(json!({ "items": items, "next_cursor": null }))
}

pub async fn list_integrations(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query("SELECT * FROM integrations WHERE user_id = ? ORDER BY created_at")
        .bind(&user.id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "provider_type": r.get::<String, _>("provider_type"),
                "display_name": r.get::<String, _>("display_name"),
                "status": r.get::<String, _>("status"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub display_name: String,
    /// For non-OAuth providers, credentials may be supplied directly.
    #[serde(default)]
    pub credentials: serde_json::Value,
}

/// Initiate a connection flow. For OAuth providers this returns an
/// authorization URL; for direct providers the integration is created
/// immediately with the supplied credentials.
pub async fn connect(
    State(state): State<AppState>,
    user: AuthUser,
    Path(provider_type): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    if !CATALOG.iter().any(|(t, _)| *t == provider_type) {
        return Err(ApiError::Unprocessable(format!("unsupported provider type '{provider_type}'")));
    }

    // Google providers use a real OAuth flow when the OAuth client is
    // configured (GOOGLE_CLIENT_ID/SECRET in the environment). Without it,
    // the user can still connect manually with an access token.
    if matches!(provider_type.as_str(), "gmail" | "gcal" | "gphotos") {
        if let Some(auth_url) = crate::services::google_oauth::authorization_url(&state, &user.id, &provider_type).await? {
            return Ok((
                axum::http::StatusCode::OK,
                Json(json!({ "oauth_url": auth_url, "flow": "oauth" })),
            ));
        }
    }
    // GitHub uses its own OAuth flow when configured.
    if provider_type == "github" {
        if let Some(auth_url) = crate::services::github_oauth::authorization_url(&state, &user.id).await? {
            return Ok((
                axum::http::StatusCode::OK,
                Json(json!({ "oauth_url": auth_url, "flow": "oauth" })),
            ));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, 'connected', ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&provider_type)
    .bind(req.display_name.trim())
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;

    // Direct providers store the supplied credentials encrypted.
    if let Some(token) = req.credentials.get("access_token").and_then(|v| v.as_str()) {
        let account_id = uuid::Uuid::new_v4().to_string();
        let (ct, nonce) = crate::services::credentials::encrypt_credential(token, &state.config.credential_encryption_key)?;
        sqlx::query(
            "INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, granted_scopes_json, status, created_at, updated_at)
             VALUES (?, ?, 'manual', ?, '[]', 'active', ?, ?)",
        )
        .bind(&account_id)
        .bind(&id)
        .bind(req.display_name.trim())
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await?;
        sqlx::query(
            "INSERT INTO encrypted_credentials (id, integration_account_id, ciphertext, nonce, key_version, updated_at)
             VALUES (?, ?, ?, ?, 1, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&account_id)
        .bind(ct)
        .bind(nonce)
        .bind(&now)
        .execute(&state.db)
        .await?;
    }

    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "integration_id": id, "status": "connected" })),
    ))
}

/// Process the OAuth callback. The `state` parameter is validated against
/// the original authorization request before any token exchange; the code
/// is exchanged for tokens with PKCE and stored encrypted.
pub async fn oauth_callback(
    State(state): State<AppState>,
    user: AuthUser,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<serde_json::Value>> {
    let code = params.get("code").ok_or_else(|| ApiError::BadRequest("missing code".into()))?;
    let oauth_state = params
        .get("state")
        .ok_or_else(|| ApiError::BadRequest("missing state".into()))?;

    // Validate the state parameter: it must be a known, unexpired flow
    // belonging to this user.
    let flow = sqlx::query("SELECT value_json FROM app_settings WHERE key = ?")
        .bind(format!("oauth_state:{oauth_state}"))
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid state".into()))?;
    let flow_json: String = flow.get("value_json");
    let flow: serde_json::Value = serde_json::from_str(&flow_json).unwrap_or(json!({}));
    if flow["user_id"].as_str() != Some(user.id.as_str()) {
        return Err(ApiError::Forbidden("state does not belong to this user".into()));
    }

    // Exchange the code for tokens with the provider-specific flow (consumes
    // the state; stores encrypted credentials).
    let (integration_id, account_id) = match flow["provider_type"].as_str() {
        Some("github") => crate::services::github_oauth::exchange_code(&state, oauth_state, code).await?,
        _ => crate::services::google_oauth::exchange_code(&state, oauth_state, code).await?,
    };
    Ok(Json(json!({
        "status": "connected",
        "integration_id": integration_id,
        "account_id": account_id,
    })))
}

async fn require_owned_integration(state: &AppState, user: &AuthUser, integration_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM integrations WHERE id = ?")
        .bind(integration_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_integration(
    State(state): State<AppState>,
    user: AuthUser,
    Path(integration_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_integration(&state, &user, &integration_id).await?;
    let row = sqlx::query("SELECT * FROM integrations WHERE id = ?")
        .bind(&integration_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "provider_type": row.get::<String, _>("provider_type"),
        "display_name": row.get::<String, _>("display_name"),
        "status": row.get::<String, _>("status"),
        "created_at": row.get::<String, _>("created_at"),
    })))
}

pub async fn delete_integration(
    State(state): State<AppState>,
    user: AuthUser,
    Path(integration_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    require_owned_integration(&state, &user, &integration_id).await?;
    sqlx::query("DELETE FROM integrations WHERE id = ?")
        .bind(&integration_id)
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_accounts(
    State(state): State<AppState>,
    user: AuthUser,
    Path(integration_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_integration(&state, &user, &integration_id).await?;
    let rows = sqlx::query("SELECT * FROM integration_accounts WHERE integration_id = ? ORDER BY created_at")
        .bind(&integration_id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "external_account_id": r.get::<String, _>("external_account_id"),
                "display_name": r.get::<String, _>("display_name"),
                "status": r.get::<String, _>("status"),
                "granted_scopes": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>("granted_scopes_json")).unwrap_or(json!([])),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

async fn require_owned_account(
    state: &AppState,
    user: &AuthUser,
    integration_id: &str,
    account_id: &str,
) -> ApiResult<()> {
    require_owned_integration(state, user, integration_id).await?;
    let row = sqlx::query("SELECT integration_id FROM integration_accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let parent: String = row.get("integration_id");
    if parent != integration_id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path((integration_id, account_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_account(&state, &user, &integration_id, &account_id).await?;
    let row = sqlx::query("SELECT * FROM integration_accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "external_account_id": row.get::<String, _>("external_account_id"),
        "display_name": row.get::<String, _>("display_name"),
        "status": row.get::<String, _>("status"),
        "granted_scopes": serde_json::from_str::<serde_json::Value>(&row.get::<String, _>("granted_scopes_json")).unwrap_or(json!([])),
    })))
}

pub async fn test_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path((integration_id, account_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_account(&state, &user, &integration_id, &account_id).await?;
    let row = sqlx::query("SELECT status FROM integration_accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_one(&state.db)
        .await?;
    let status: String = row.get("status");
    Ok(Json(json!({ "ok": status == "active", "account_status": status })))
}

pub async fn reconnect_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path((integration_id, account_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_account(&state, &user, &integration_id, &account_id).await?;
    sqlx::query("UPDATE integration_accounts SET status = 'active', updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&account_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "status": "active" })))
}

pub async fn account_capabilities(
    State(state): State<AppState>,
    user: AuthUser,
    Path((integration_id, account_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_account(&state, &user, &integration_id, &account_id).await?;
    let row = sqlx::query(
        "SELECT i.provider_type, a.status, a.granted_scopes_json
         FROM integration_accounts a JOIN integrations i ON i.id = a.integration_id
         WHERE a.id = ?",
    )
    .bind(&account_id)
    .fetch_one(&state.db)
    .await?;
    let provider_type: String = row.get("provider_type");
    let status: String = row.get("status");
    let scopes: Vec<String> = serde_json::from_str(&row.get::<String, _>("granted_scopes_json")).unwrap_or_default();

    // Capabilities reflect implemented operations AND current account state.
    let operations: Vec<&str> = CATALOG
        .iter()
        .find(|(t, _)| *t == provider_type)
        .map(|(_, ops)| ops.to_vec())
        .unwrap_or_default();
    let available = if status == "active" { operations.clone() } else { vec![] };
    Ok(Json(json!({
        "provider_type": provider_type,
        "account_status": status,
        "granted_scopes": scopes,
        "operations": available,
    })))
}