//! Integration tests for the WebSocket event channel (spec section 4).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};

use serde_json::{json, Value};
use tower::ServiceExt;

use crate::{api, events::EventEnvelope, state::AppState};

async fn test_state() -> AppState {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = crate::db::connect(&url).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    // Keep the tempdir alive for the process lifetime: SQLite reopens the
    // file lazily, so it must still exist while the pool is in use.
    std::mem::forget(dir);
    AppState {
        db: pool,
        config: crate::config::AppConfig {
            database_url: url,
            bind_addr: "127.0.0.1:0".into(),
            skills_dir: "data/skills".into(),
            max_upload_bytes: 1024,
            session_secret: "test-secret".into(),
            workspace_dir: "data/workspace".into(),
            max_workspace_file_bytes: 1024 * 1024,
            credential_encryption_key: "k".into(),
        },
        events: std::sync::Arc::new(crate::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
    }
}

/// Create the admin user and log in, returning a session token.
async fn setup_and_login(state: &AppState) -> String {
    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/setup")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "email": "admin@example.com", "display_name": "Admin", "password": "supersecret1" })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "email": "admin@example.com", "password": "supersecret1" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    body["session_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn ticket_requires_authentication() {
    let state = test_state().await;
    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/events/ticket")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "session_token": "bad" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ticket_issued_for_valid_session() {
    let state = test_state().await;
    let token = setup_and_login(&state).await;

    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/events/ticket")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "session_token": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(!body["ticket"].as_str().unwrap().is_empty());
}

/// Full flow: issue ticket, connect over a real WebSocket, receive a
/// published event end-to-end.
#[tokio::test]
async fn websocket_receives_published_events() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let state = test_state().await;
    let token = setup_and_login(&state).await;

    // Issue the ticket over REST.
    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/events/ticket")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "session_token": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let ticket = body["ticket"].as_str().unwrap().to_string();

    // Start a real server: tower's oneshot cannot perform WebSocket upgrades.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_state = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, api::router(server_state)).await.unwrap();
    });

    // Connect with the single-use ticket.
    let (mut ws, _) = tokio_tungstenite::connect_async(
        format!("ws://{addr}/api/v1/events?ticket={ticket}").into_client_request().unwrap(),
    )
    .await
    .unwrap();

    // Publish an event and expect it on the socket.
    crate::api::events::publish(
        &state,
        "task.started",
        json!({ "task_id": "t-1", "task_run_id": "r-1" }),
    );
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for event")
        .unwrap()
        .unwrap();
    let envelope: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(envelope["event_type"], "task.started");
    assert_eq!(envelope["data"]["task_id"], "t-1");
    assert!(envelope["event_id"].is_string());
    assert!(envelope["timestamp"].is_string());
}

/// An invalid ticket is rejected before the upgrade completes.
#[tokio::test]
async fn websocket_rejects_invalid_ticket() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let state = test_state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_state = state.clone();
    tokio::spawn(async move {
        axum::serve(listener, api::router(server_state)).await.unwrap();
    });

    let request = format!("ws://{addr}/api/v1/events?ticket=bogus").into_client_request().unwrap();
    let result = tokio_tungstenite::connect_async(request).await;
    assert!(result.is_err(), "expected rejection for invalid ticket");
}

/// A ticket can only be used once: the second upgrade is rejected.
#[tokio::test]
async fn websocket_ticket_is_single_use() {
    let state = test_state().await;
    let token = setup_and_login(&state).await;

    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/events/ticket")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "session_token": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let ticket = body["ticket"].as_str().unwrap().to_string();

    // First use succeeds.
    assert!(state.ws_tickets.consume(&ticket).is_some());
    // Second use fails.
    assert!(state.ws_tickets.consume(&ticket).is_none());
}

/// Envelope serialization matches the documented shape.
#[test]
fn envelope_serialization_shape() {
    let envelope = EventEnvelope::new("notification.created", json!({ "notification_id": "n-1" }));
    let value: Value = serde_json::to_value(&envelope).unwrap();
    assert!(value.get("event_id").is_some());
    assert!(value.get("event_type").is_some());
    assert!(value.get("timestamp").is_some());
    assert!(value.get("data").is_some());
}






