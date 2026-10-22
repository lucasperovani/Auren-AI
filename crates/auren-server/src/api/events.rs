//! WebSocket endpoint `/api/v1/events` (spec: docs/PROMPT.md section 4).
//!
//! Authentication model: the client first calls `POST /api/v1/events/ticket`
//! with its session token; the server returns a short-lived, single-use
//! ticket. The client then presents the ticket in the upgrade request
//! (`?ticket=...`). No reusable session token is ever placed in a URL.

use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    Json,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

use crate::{
    error::{ApiError, ApiResult},
    events::{EventEnvelope, WsTicket},
    state::AppState,
};

/// Ticket time-to-live in seconds. Short by design: the ticket is consumed
/// immediately after issuance by the same client.
const TICKET_TTL_SECS: i64 = 30;

/// Server-side ping interval to keep intermediaries from dropping idle
/// connections and to detect dead peers.
const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub struct TicketRequest {
    pub session_token: String,
}

/// Issue a single-use WebSocket upgrade ticket for the authenticated user.
pub async fn issue_ticket(
    State(state): State<AppState>,
    Json(req): Json<TicketRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let user = super::auth::authenticate(&state, &req.session_token).await?;
    state.ws_tickets.prune();
    let ticket = state
        .ws_tickets
        .issue(user["id"].as_str().unwrap_or_default(), TICKET_TTL_SECS);
    Ok(Json(serde_json::json!({
        "ticket": ticket.ticket_id,
        "expires_at": ticket.expires_at.to_rfc3339(),
    })))
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub ticket: String,
}

/// WebSocket upgrade for `/api/v1/events`. The ticket is consumed atomically;
/// unknown, expired, or already-used tickets are rejected before the upgrade.
pub async fn events_ws(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<axum::response::Response> {
    let ticket: WsTicket = state
        .ws_tickets
        .consume(&query.ticket)
        .ok_or(ApiError::Unauthorized)?;

    Ok(upgrade.on_upgrade(move |socket| handle_socket(state, socket, ticket)))
}

async fn handle_socket(state: AppState, socket: WebSocket, ticket: WsTicket) {
    let (mut sender, mut receiver) = socket.split();
    let mut event_rx = state.events.subscribe();

    // Forward live events to the client.
    let mut send_task = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    match event {
                        Ok(envelope) => {
                            if sender.send(Message::Text(serde_json::to_string(&envelope).unwrap_or_default().into())).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped = n, "websocket client lagged; events dropped");
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ping.tick() => {
                    if sender.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Read from the client: only pongs and close frames are expected. Any
    // other message closes the connection (the channel is one-way).
    let user_id = ticket.user_id.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Pong(_) | Message::Ping(_) => continue,
                Message::Close(_) => break,
                _ => {
                    tracing::debug!(user_id = %user_id, "unexpected client frame; closing");
                    break;
                }
            }
        }
    });

    // When either task finishes, tear down both.
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}

/// Publish an event on the bus. Services call this after persisting state.
pub fn publish(state: &AppState, event_type: &str, data: serde_json::Value) {
    let envelope = EventEnvelope::new(event_type, data);
    let receivers = state.events.publish(envelope);
    tracing::debug!(event_type, receivers, "event published");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ticket_endpoint_requires_valid_session() {
        let state = test_state().await;
        let result = issue_ticket(
            State(state),
            Json(TicketRequest { session_token: "invalid".into() }),
        )
        .await;
        assert!(result.is_err());
    }

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/test.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        AppState {
            db: pool,
            config: crate::config::AppConfig {
                database_url: url,
                bind_addr: "127.0.0.1:0".into(),
                skills_dir: "data/skills".into(),
                max_upload_bytes: 1024,
                session_secret: "test-secret".into(),
            },
            events: std::sync::Arc::new(crate::events::EventBus::new()),
            ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
        }
    }
}