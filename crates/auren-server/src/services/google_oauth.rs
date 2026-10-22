//! Google OAuth 2.0 with PKCE (spec part1 section 14).
//!
//! The OAuth client is configured through the environment:
//! `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`. When configured, the
//! Integrations view can start a real authorization flow; the callback
//! exchanges the code for tokens and stores them encrypted.

use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::{error::{ApiError, ApiResult}, state::AppState};

/// Google's OAuth 2.0 endpoints.
const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Least-privilege scopes per provider (spec section 14: request additional
/// permissions only when required).
fn scopes_for(provider_type: &str) -> &'static [&'static str] {
    match provider_type {
        "gmail" => &["https://www.googleapis.com/auth/gmail.readonly", "https://www.googleapis.com/auth/gmail.send"],
        "gcal" => &["https://www.googleapis.com/auth/calendar.events"],
        "gphotos" => &["https://www.googleapis.com/auth/photoslibrary.readonly"],
        _ => &[],
    }
}

/// The redirect URI the application listens on.
fn redirect_uri(state: &AppState) -> String {
    let bind = &state.config.bind_addr;
    // Public deployments sit behind a reverse proxy; the configured public
    // origin takes precedence.
    std::env::var("AUREN_PUBLIC_ORIGIN").unwrap_or_else(|_| format!("http://{bind}"))
}

/// Build the Google authorization URL with PKCE. Returns `None` when the
/// OAuth client is not configured (manual token connection still works).
pub async fn authorization_url(
    state: &AppState,
    user_id: &str,
    provider_type: &str,
) -> ApiResult<Option<String>> {
    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    if client_id.is_empty() {
        return Ok(None);
    }
    let client_secret = std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default();

    // PKCE: a random verifier; only its SHA-256 challenge travels in the URL.
    let mut verifier_bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut verifier_bytes);
    let verifier = base64_url(&verifier_bytes);
    let challenge = base64_url(&Sha256::digest(verifier.as_bytes()));

    // The state parameter binds the callback to this flow (single use).
    let oauth_state = uuid::Uuid::new_v4().to_string();
    let flow = json!({
        "user_id": user_id,
        "provider_type": provider_type,
        "code_verifier": verifier,
        "client_secret": client_secret,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    // The flow record is stored server-side; the client secret never
    // travels to the browser.
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

    let scopes = scopes_for(provider_type).join(" ");
    let url = format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        urlencode(&client_id),
        urlencode(&redirect_uri(state)),
        urlencode(&scopes),
        urlencode(&oauth_state),
        challenge,
    );
    Ok(Some(url))
}

/// Exchange the authorization code for tokens and store them encrypted on a
/// new integration account.
pub async fn exchange_code(
    state: &AppState,
    oauth_state: &str,
    code: &str,
) -> ApiResult<(String, String)> {
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
    let provider_type = flow["provider_type"].as_str().unwrap_or_default().to_string();
    let verifier = flow["code_verifier"].as_str().unwrap_or_default().to_string();
    let client_secret = flow["client_secret"].as_str().unwrap_or_default().to_string();
    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();

    // Token exchange with the PKCE verifier.
    let response = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("code", code),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code_verifier", verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri(state).as_str()),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| {
            tracing::debug!(error = %e, "token exchange failed");
            ApiError::ServiceUnavailable
        })?;
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(status = %body, "google token exchange rejected");
        return Err(ApiError::Conflict("token exchange failed; check the OAuth client configuration".into()));
    }
    let tokens: Value = response.json().await.map_err(|_| ApiError::ServiceUnavailable)?;
    let access_token = tokens["access_token"].as_str().ok_or_else(|| {
        ApiError::Conflict("token response missing access_token".into())
    })?;
    let refresh_token = tokens["refresh_token"].as_str().unwrap_or_default().to_string();

    // Create the integration and account with encrypted credentials.
    let integration_id = uuid::Uuid::new_v4().to_string();
    let account_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, 'connected', ?, ?)",
    )
    .bind(&integration_id)
    .bind(&user_id)
    .bind(&provider_type)
    .bind(format!("Google {}", provider_type.to_uppercase()))
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;

    // Access and refresh tokens are stored as one encrypted JSON blob.
    let credentials = json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expires_at": tokens["expires_in"].as_u64().map(|s| {
            (chrono::Utc::now() + chrono::Duration::seconds(s as i64)).to_rfc3339()
        }),
    });
    let (ct, nonce) = crate::services::credentials::encrypt_credential(
        &credentials.to_string(),
        &state.config.credential_encryption_key,
    )?;
    sqlx::query(
        "INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, granted_scopes_json, status, created_at, updated_at)
         VALUES (?, ?, 'google', ?, ?, 'active', ?, ?)",
    )
    .bind(&account_id)
    .bind(&integration_id)
    .bind("Google account")
    .bind(json!(scopes_for(&provider_type)).to_string())
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

/// Refresh an expired access token using the stored refresh token.
pub async fn refresh_access_token(state: &AppState, account_id: &str) -> ApiResult<String> {
    let row = sqlx::query(
        "SELECT c.ciphertext, c.nonce FROM encrypted_credentials c WHERE c.integration_account_id = ?",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let ct: Vec<u8> = row.get("ciphertext");
    let nonce: Vec<u8> = row.get("nonce");
    let decrypted = crate::services::credentials::decrypt_credential(&ct, &nonce, &state.config.credential_encryption_key)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("credential decryption failed")))?;
    let credentials: Value = serde_json::from_str(&decrypted)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("corrupt credentials")))?;
    let refresh_token = credentials["refresh_token"].as_str().unwrap_or_default();
    if refresh_token.is_empty() {
        return Err(ApiError::Conflict("no refresh token stored; reconnect the account".into()));
    }

    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    let client_secret = std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default();
    let response = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;
    if !response.status().is_success() {
        return Err(ApiError::Conflict("token refresh failed; reconnect the account".into()));
    }
    let tokens: Value = response.json().await.map_err(|_| ApiError::ServiceUnavailable)?;
    let access_token = tokens["access_token"].as_str().ok_or_else(|| {
        ApiError::Conflict("refresh response missing access_token".into())
    })?;

    // Update the stored credentials.
    let mut updated = credentials.clone();
    updated["access_token"] = json!(access_token);
    updated["expires_at"] = json!(tokens["expires_in"].as_u64().map(|s| {
        (chrono::Utc::now() + chrono::Duration::seconds(s as i64)).to_rfc3339()
    }));
    let (ct, nonce) = crate::services::credentials::encrypt_credential(
        &updated.to_string(),
        &state.config.credential_encryption_key,
    )?;
    sqlx::query("UPDATE encrypted_credentials SET ciphertext = ?, nonce = ?, updated_at = ? WHERE integration_account_id = ?")
        .bind(ct)
        .bind(nonce)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(account_id)
        .execute(&state.db)
        .await?;
    Ok(access_token.to_string())
}

fn base64_url(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}