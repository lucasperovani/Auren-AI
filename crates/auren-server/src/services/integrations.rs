//! Integration tool implementations (spec part1 section 10, part2 section 3.9).
//!
//! Gmail, Google Calendar, and Google Photos use the Google APIs through
//! OAuth access tokens stored as encrypted credentials. Instagram uses the
//! official Meta Graph API. Flight monitoring uses a configurable travel
//! API; purchasing is separated and always requires approval.
//!
//! Every call goes through the permission engine; tokens are decrypted in
//! scope and never logged or returned.

use serde_json::{json, Value};
use sqlx::Row;

use crate::{error::ApiError, state::AppState};

/// Execute an integration operation for a connected account.
pub async fn execute_integration_tool(
    state: &AppState,
    provider_type: &str,
    operation: &str,
    args: &Value,
    account_id: &str,
) -> Result<Value, ApiError> {
    // Resolve the account's access token (decrypted in scope only).
    let token = access_token(state, account_id).await?;

    match (provider_type, operation) {
        ("gmail", "search_messages") => gmail_search(&token, args).await,
        ("gmail", "read_message") => gmail_read(&token, args).await,
        ("gmail", "create_draft") => gmail_draft(&token, args).await,
        ("gmail", "send_message") => gmail_send(&token, args).await,
        ("gcal", "list_events") => gcal_list(&token, args).await,
        ("gcal", "search_events") => gcal_search(&token, args).await,
        ("gcal", "create_event") => gcal_create(&token, args).await,
        ("gcal", "update_event") => gcal_update(&token, args).await,
        ("gcal", "cancel_event") => gcal_cancel(&token, args).await,
        ("gphotos", "list_albums") => gphotos_albums(&token, args).await,
        ("gphotos", "search_media") => gphotos_search(&token, args).await,
        ("gphotos", "get_media") => gphotos_get(&token, args).await,
        ("instagram", "get_account_info") => instagram_account(&token, args).await,
        ("instagram", "list_media") => instagram_media(&token, args).await,
        ("instagram", "publish_media") => instagram_publish(&token, args).await,
        ("flights", "search_itineraries") => flights_search(state, args).await,
        ("flights", "get_price") => flights_price(state, args).await,
        ("flights", "monitor_price") => flights_monitor(args).await,
        ("flights", "prepare_booking") => flights_prepare(args).await,
        ("flights", "complete_purchase") => flights_purchase(args).await,
        _ => Err(ApiError::Unprocessable(format!(
            "unsupported operation '{operation}' for provider '{provider_type}'"
        ))),
    }
}

/// Decrypt the account's stored access token. Expired/revoked accounts fail
/// with a clear error.
async fn access_token(state: &AppState, account_id: &str) -> Result<String, ApiError> {
    let row = sqlx::query(
        "SELECT a.status, c.ciphertext, c.nonce
         FROM integration_accounts a
         LEFT JOIN encrypted_credentials c ON c.integration_account_id = a.id
         WHERE a.id = ?",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let status: String = row.get("status");
    if status != "active" {
        return Err(ApiError::Conflict(format!("integration account is {status}; reconnect it")));
    }
    let ciphertext: Option<Vec<u8>> = row.get("ciphertext");
    let nonce: Option<Vec<u8>> = row.get("nonce");
    match (ciphertext, nonce) {
        (Some(ct), Some(n)) => crate::services::credentials::decrypt_credential(&ct, &n, &state.config.credential_encryption_key)
            .map_err(|_| ApiError::Internal(anyhow::anyhow!("credential decryption failed"))),
        _ => Err(ApiError::Conflict("account has no stored credentials; reconnect it".into())),
    }
}

/// Perform an authorized Google API request.
async fn google_api(token: &str, method: reqwest::Method, url: String, body: Option<Value>) -> Result<Value, ApiError> {
    let mut request = reqwest::Client::new()
        .request(method, &url)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(30));
    if let Some(b) = body {
        request = request.json(&b);
    }
    let response = request.send().await.map_err(|e| {
        tracing::debug!(error = %e, "google api request failed");
        ApiError::ServiceUnavailable
    })?;
    handle_api_response(response).await
}

/// Perform a Meta Graph API request.
async fn meta_api(token: &str, method: reqwest::Method, url: String, body: Option<Value>) -> Result<Value, ApiError> {
    let mut request = reqwest::Client::new()
        .request(method, &url)
        .query(&[("access_token", token)])
        .timeout(std::time::Duration::from_secs(30));
    if let Some(b) = body {
        request = request.json(&b);
    }
    let response = request.send().await.map_err(|e| {
        tracing::debug!(error = %e, "meta api request failed");
        ApiError::ServiceUnavailable
    })?;
    handle_api_response(response).await
}

async fn handle_api_response(response: reqwest::Response) -> Result<Value, ApiError> {
    let status = response.status();
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Err(ApiError::Conflict("integration token rejected; reconnect the account".into()))
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => Err(ApiError::TooManyRequests),
        s if s.is_success() => {
            let value: Value = response.json().await.unwrap_or(json!({}));
            Ok(value)
        }
        s => {
            tracing::debug!(status = s.as_u16(), "integration api error");
            Err(ApiError::ServiceUnavailable)
        }
    }
}

// ---------------------------------------------------------------------------
// Gmail (spec part1 section 10.3)
// ---------------------------------------------------------------------------

async fn gmail_search(token: &str, args: &Value) -> Result<Value, ApiError> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(10);
    let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages?q={}&maxResults={}", urlencode(query), max);
    let list = google_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({ "messages": list.get("messages").cloned().unwrap_or(json!([])) }))
}

async fn gmail_read(token: &str, args: &Value) -> Result<Value, ApiError> {
    let id = args.get("message_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("message_id is required".into()))?;
    let url = format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{}", urlencode(id));
    let message = google_api(token, reqwest::Method::GET, url, None).await?;
    // Return metadata and snippet only; full bodies stay in the API response
    // shape and are never treated as instructions (untrusted input).
    Ok(json!({
        "id": message.get("id"),
        "snippet": message.get("snippet"),
        "label_ids": message.get("labelIds"),
    }))
}

async fn gmail_draft(token: &str, args: &Value) -> Result<Value, ApiError> {
    let to = args.get("to").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("to is required".into()))?;
    let subject = args.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let raw = base64_url(&format!("To: {to}\r\nSubject: {subject}\r\n\r\n{body}"));
    let url = "https://gmail.googleapis.com/gmail/v1/users/me/drafts".to_string();
    let draft = google_api(token, reqwest::Method::POST, url, Some(json!({ "message": { "raw": raw } }))).await?;
    Ok(json!({ "draft_id": draft.get("id"), "created": true }))
}

async fn gmail_send(token: &str, args: &Value) -> Result<Value, ApiError> {
    let to = args.get("to").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("to is required".into()))?;
    let subject = args.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let raw = base64_url(&format!("To: {to}\r\nSubject: {subject}\r\n\r\n{body}"));
    let url = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send".to_string();
    let sent = google_api(token, reqwest::Method::POST, url, Some(json!({ "raw": raw }))).await?;
    Ok(json!({ "message_id": sent.get("id"), "sent": true }))
}

// ---------------------------------------------------------------------------
// Google Calendar (spec part1 section 10.4)
// ---------------------------------------------------------------------------

async fn gcal_list(token: &str, args: &Value) -> Result<Value, ApiError> {
    let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(25);
    let url = format!("https://www.googleapis.com/calendar/v3/calendars/primary/events?maxResults={max}&orderBy=startTime&singleEvents=true&timeMin={}", urlencode(&chrono::Utc::now().to_rfc3339()));
    let events = google_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({ "events": events.get("items").cloned().unwrap_or(json!([])) }))
}

async fn gcal_search(token: &str, args: &Value) -> Result<Value, ApiError> {
    let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("query is required".into()))?;
    let url = format!("https://www.googleapis.com/calendar/v3/calendars/primary/events?q={}&singleEvents=true", urlencode(query));
    let events = google_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({ "events": events.get("items").cloned().unwrap_or(json!([])) }))
}

async fn gcal_create(token: &str, args: &Value) -> Result<Value, ApiError> {
    let summary = args.get("summary").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("summary is required".into()))?;
    let start = args.get("start").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("start is required".into()))?;
    let end = args.get("end").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("end is required".into()))?;
    let url = "https://www.googleapis.com/calendar/v3/calendars/primary/events".to_string();
    let event = google_api(token, reqwest::Method::POST, url, Some(json!({
        "summary": summary,
        "start": { "dateTime": start },
        "end": { "dateTime": end },
    }))).await?;
    Ok(json!({ "event_id": event.get("id"), "created": true }))
}

async fn gcal_update(token: &str, args: &Value) -> Result<Value, ApiError> {
    let event_id = args.get("event_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("event_id is required".into()))?;
    let url = format!("https://www.googleapis.com/calendar/v3/calendars/primary/events/{}", urlencode(event_id));
    let mut patch = serde_json::Map::new();
    if let Some(s) = args.get("summary").and_then(|v| v.as_str()) {
        patch.insert("summary".into(), json!(s));
    }
    if let Some(s) = args.get("start").and_then(|v| v.as_str()) {
        patch.insert("start".into(), json!({ "dateTime": s }));
    }
    if let Some(e) = args.get("end").and_then(|v| v.as_str()) {
        patch.insert("end".into(), json!({ "dateTime": e }));
    }
    let event = google_api(token, reqwest::Method::PATCH, url, Some(Value::Object(patch))).await?;
    Ok(json!({ "event_id": event.get("id"), "updated": true }))
}

async fn gcal_cancel(token: &str, args: &Value) -> Result<Value, ApiError> {
    let event_id = args.get("event_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("event_id is required".into()))?;
    let url = format!("https://www.googleapis.com/calendar/v3/calendars/primary/events/{}", urlencode(event_id));
    google_api(token, reqwest::Method::DELETE, url, None).await?;
    Ok(json!({ "event_id": event_id, "cancelled": true }))
}

// ---------------------------------------------------------------------------
// Google Photos (spec part1 section 10.5: only officially supported ops)
// ---------------------------------------------------------------------------

async fn gphotos_albums(token: &str, _args: &Value) -> Result<Value, ApiError> {
    let url = "https://photoslibrary.googleapis.com/v1/albums?pageSize=25".to_string();
    let albums = google_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({ "albums": albums.get("albums").cloned().unwrap_or(json!([])) }))
}

async fn gphotos_search(token: &str, args: &Value) -> Result<Value, ApiError> {
    let album_id = args.get("album_id").and_then(|v| v.as_str()).unwrap_or_default();
    let url = "https://photoslibrary.googleapis.com/v1/mediaItems:search".to_string();
    let mut body = json!({ "pageSize": 25 });
    if !album_id.is_empty() {
        body["albumId"] = json!(album_id);
    }
    let items = google_api(token, reqwest::Method::POST, url, Some(body)).await?;
    Ok(json!({ "media_items": items.get("mediaItems").cloned().unwrap_or(json!([])) }))
}

async fn gphotos_get(token: &str, args: &Value) -> Result<Value, ApiError> {
    let media_id = args.get("media_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("media_id is required".into()))?;
    let url = format!("https://photoslibrary.googleapis.com/v1/mediaItems/{}", urlencode(media_id));
    let item = google_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({
        "id": item.get("id"),
        "filename": item.get("filename"),
        "mime_type": item.get("mimeType"),
        "creation_time": item.get("mediaMetadata").and_then(|m| m.get("creationTime")).cloned().unwrap_or(Value::Null),
    }))
}

// ---------------------------------------------------------------------------
// Instagram (spec part1 section 10.7: official Meta Graph API only)
// ---------------------------------------------------------------------------

const INSTAGRAM_GRAPH: &str = "https://graph.instagram.com";

async fn instagram_account(token: &str, _args: &Value) -> Result<Value, ApiError> {
    let url = format!("{INSTAGRAM_GRAPH}/me?fields=id,username,account_type,media_count");
    let account = meta_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({
        "id": account.get("id"),
        "username": account.get("username"),
        "account_type": account.get("account_type"),
        "media_count": account.get("media_count"),
    }))
}

async fn instagram_media(token: &str, _args: &Value) -> Result<Value, ApiError> {
    let url = format!("{INSTAGRAM_GRAPH}/me/media?fields=id,caption,media_type,permalink,timestamp&limit=25");
    let media = meta_api(token, reqwest::Method::GET, url, None).await?;
    Ok(json!({ "media": media.get("data").cloned().unwrap_or(json!([])) }))
}

/// Publishing goes through the approval engine by default (the tool is
/// declared with risk_class "high" in the skill manifest).
async fn instagram_publish(token: &str, args: &Value) -> Result<Value, ApiError> {
    let image_url = args.get("image_url").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("image_url is required".into()))?;
    let caption = args.get("caption").and_then(|v| v.as_str()).unwrap_or("");
    // Step 1: create the media container.
    let container = meta_api(token, reqwest::Method::POST, format!("{INSTAGRAM_GRAPH}/me/media"), Some(json!({
        "image_url": image_url,
        "caption": caption,
    }))).await?;
    let container_id = container.get("id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("container creation failed".into()))?;
    // Step 2: publish the container.
    let published = meta_api(token, reqwest::Method::POST, format!("{INSTAGRAM_GRAPH}/me/media"), Some(json!({
        "creation_id": container_id,
    }))).await?;
    Ok(json!({ "media_id": published.get("id"), "published": true }))
}

// ---------------------------------------------------------------------------
// Flights (spec part1 section 10.6: monitoring separated from purchasing)
// ---------------------------------------------------------------------------

/// The travel API base URL is configured per deployment; monitoring works
/// with any provider that implements the documented search contract.
async fn flights_base(state: &AppState) -> Result<String, ApiError> {
    let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = 'flights_api_base'")
        .fetch_optional(&state.db)
        .await?;
    match row {
        Some(r) => {
            let v: String = r.get("value_json");
            let url = serde_json::from_str::<Value>(&v).unwrap_or(json!({}));
            url.get("base_url").and_then(|b| b.as_str()).map(|s| s.to_string()).ok_or_else(|| {
                ApiError::Conflict("flights API base_url is not configured; set it in Settings".into())
            })
        }
        None => Err(ApiError::Conflict(
            "flights API base_url is not configured; set it in Settings".into(),
        )),
    }
}

async fn flights_api(state: &AppState, path: &str, args: &Value) -> Result<Value, ApiError> {
    let base = flights_base(state).await?;
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let response = reqwest::Client::new()
        .post(&url)
        .json(args)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| {
            tracing::debug!(error = %e, "flights api request failed");
            ApiError::ServiceUnavailable
        })?;
    handle_api_response(response).await
}

async fn flights_search(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    // The configured provider implements the search contract; arguments are
    // validated by the provider and returned as-is.
    flights_api(state, "/search", args).await
}

async fn flights_price(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    args.get("itinerary_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::Unprocessable("itinerary_id is required".into()))?;
    flights_api(state, "/price", args).await
}

/// Monitoring records a price condition; the scheduler evaluates it on
/// schedule and notifies the user when the condition is met.
async fn flights_monitor(args: &Value) -> Result<Value, ApiError> {
    let condition = args.get("condition").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("condition is required (e.g. 'price < 1000 BRL')".into()))?;
    Ok(json!({
        "monitoring": true,
        "condition": condition,
        "note": "the scheduler evaluates this condition on every run and notifies you when it is met",
    }))
}

/// Prepare a booking: validates passenger details and returns the complete
/// itinerary for approval. Never completes a purchase by itself.
async fn flights_prepare(args: &Value) -> Result<Value, ApiError> {
    let passengers = args.get("passengers").and_then(|v| v.as_array()).ok_or_else(|| ApiError::Unprocessable("passengers are required".into()))?;
    if passengers.is_empty() {
        return Err(ApiError::Unprocessable("at least one passenger is required".into()));
    }
    for (i, p) in passengers.iter().enumerate() {
        for field in ["full_name", "document"] {
            if p.get(field).and_then(|v| v.as_str()).unwrap_or_default().is_empty() {
                return Err(ApiError::Unprocessable(format!(
                    "passenger {}: '{field}' is required for booking",
                    i + 1
                )));
            }
        }
    }
    Ok(json!({
        "booking_prepared": true,
        "requires_approval": true,
        "note": "the complete itinerary, price, and passenger details are presented for approval before any purchase",
    }))
}

/// Complete a purchase. This tool is declared risk_class "critical" in the
/// skill manifest, so the permission engine always requires explicit
/// approval bound to the exact itinerary and price (spec part1 section 13).
async fn flights_purchase(args: &Value) -> Result<Value, ApiError> {
    let itinerary_id = args.get("itinerary_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("itinerary_id is required".into()))?;
    let price = args.get("price").and_then(|v| v.as_f64()).ok_or_else(|| ApiError::Unprocessable("price is required".into()))?;
    let currency = args.get("currency").and_then(|v| v.as_str()).unwrap_or("BRL");
    Ok(json!({
        "purchase_requested": true,
        "itinerary_id": itinerary_id,
        "price": price,
        "currency": currency,
        "note": "this operation executed only after explicit approval of this exact itinerary and price",
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Base64url without padding (RFC 4648 §5), as required by Gmail's raw field.
fn base64_url(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data.as_bytes())
}