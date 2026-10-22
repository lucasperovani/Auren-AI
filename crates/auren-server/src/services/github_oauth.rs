//! GitHub OAuth 2.0 (spec part1 section 14).
//!
//! Configured through `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`. The
//! callback exchanges the code for an access token stored encrypted.

use serde_json::{json, Value};
use sqlx::Row;

use crate::{error::{ApiError, ApiResult}, state::AppState};

const AUTH_ENDPOINT: &str = "https://github.com/login/oauth/authorize";
const TOKEN_ENDPOINT: &str = "https://github.com/login/oauth/access_token";

/// Least-privilege scopes for repository access.
const SCOPES: &str = "repo,read:org";

fn redirect_uri(state: &AppState) -> String {
    std::env::var("AUREN_PUBLIC_ORIGIN").unwrap_or_else(|_| format!("http://{}", state.config.bind_addr))
}

/// Build the GitHub authorization URL. Returns `None` when the OAuth client
/// is not configured (manual token connection still works).
pub async fn authorization_url(state: &AppState, user_id: &str) -> ApiResult<Option<String>> {
    let client_id = std::env::var("GITHUB_CLIENT_ID").unwrap_or_default();
    if client_id.is_empty() {
        return Ok(None);
    }
    let client_secret = std::env::var("GITHUB_CLIENT_SECRET").unwrap_or_default();

    // GitHub supports `state` for CSRF; PKCE is not part of its web flow.
    let oauth_state = uuid::Uuid::new_v4().to_string();
    let flow = json!({
        "user_id": user_id,
        "provider_type": "github",
        "client_secret": client_secret,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    sqlx::query(
        "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(format!("oauth_state:{oauth_state}"))
    .bind(flow.to_string())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.db)
    .await
    .map_err(ApiError::from)?;

    Ok(Some(format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&scope={}&state={}",
        urlencode(&client_id),
        urlencode(&redirect_uri(state)),
        urlencode(SCOPES),
        urlencode(&oauth_state),
    )))
}

/// Exchange the authorization code for an access token and store it
/// encrypted on a new integration account.
pub async fn exchange_code(state: &AppState, oauth_state: &str, code: &str) -> ApiResult<(String, String)> {
    let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = ?")
        .bind(format!("oauth_state:{oauth_state}"))
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid state".into()))?;
    let raw: String = row.get("value_json");
    let flow: Value = serde_json::from_str(&raw)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("corrupt oauth flow")))?;
    // Single use: consume the state immediately.
    sqlx::query("DELETE FROM app_settings WHERE key = ?")
        .bind(format!("oauth_state:{oauth_state}"))
        .execute(&state.db)
        .await?;

    let user_id = flow["user_id"].as_str().unwrap_or_default().to_string();
    let client_secret = flow["client_secret"].as_str().unwrap_or_default().to_string();
    let client_id = std::env::var("GITHUB_CLIENT_ID").unwrap_or_default();

    let response = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri(state).as_str()),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;
    let tokens: Value = response.json().await.map_err(|_| ApiError::ServiceUnavailable)?;
    let access_token = tokens["access_token"]
        .as_str()
        .ok_or_else(|| ApiError::Conflict("token exchange failed; check the OAuth client configuration".into()))?
        .to_string();

    // Create the integration and account with encrypted credentials.
    let integration_id = uuid::Uuid::new_v4().to_string();
    let account_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at)
         VALUES (?, ?, 'github', 'GitHub', 'connected', ?, ?)",
    )
    .bind(&integration_id)
    .bind(&user_id)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    let (ct, nonce) = crate::services::credentials::encrypt_credential(
        &access_token,
        &state.config.credential_encryption_key,
    )?;
    sqlx::query(
        "INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, granted_scopes_json, status, created_at, updated_at)
         VALUES (?, ?, 'github', 'GitHub account', ?, 'active', ?, ?)",
    )
    .bind(&account_id)
    .bind(&integration_id)
    .bind(json!(SCOPES.split(", ").collect::<Vec<_>>()).to_string())
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

    Ok((integration_id, account_id))
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}