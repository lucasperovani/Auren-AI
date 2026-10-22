pub mod auth;
pub mod auth_user;
pub mod agents;
pub mod approvals;
pub mod conversations;
pub mod events;
pub mod health;
pub mod integrations;
pub mod llm;
pub mod misc;
pub mod pagination;
pub mod rate_limit;
pub mod skills;
pub mod task_plans;
pub mod task_runs;
pub mod tasks;

#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod integration_tests;

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, get as get_route, post},
    Router,
};

use crate::state::AppState;

/// Build the full application router: versioned API, health probes, and the
/// static frontend served from the same server (spec section 18).
///
/// Unknown non-API paths return the SPA entry point with 200 so client-side
/// routing works on deep links.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get_route(crate::api::health::liveness))
        .route("/health/ready", get_route(crate::api::health::readiness))
        .nest("/api/v1", api_v1(state.clone()))
        .fallback(spa_fallback)
        .with_state(state)
}

/// SPA fallback handler: serve static files when they exist, index.html
/// otherwise (client-side routing on deep links).
async fn spa_fallback(uri: axum::http::Uri) -> Response {
    let frontend_dir = std::env::var("AUREN_FRONTEND_DIR").unwrap_or_else(|_| "frontend".to_string());
    serve_frontend(&frontend_dir, uri.path()).await
}

/// Serve a static frontend file; unknown paths return the SPA entry point
/// with 200 so client-side routing works on deep links.
async fn serve_frontend(frontend_dir: &str, path: &str) -> Response {
    // Reject traversal and resolve the file inside the frontend directory.
    let relative = path.trim_start_matches('/');
    if relative.split('/').any(|c| c == "..") {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let resolved = std::path::Path::new(frontend_dir).join(relative);
    if relative.is_empty() || !resolved.is_file() {
        // SPA entry point.
        return match tokio::fs::read(std::path::Path::new(frontend_dir).join("index.html")).await {
            Ok(bytes) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], bytes).into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "frontend not found").into_response(),
        };
    }
    let content_type = match resolved.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") | Some("mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    };
    match tokio::fs::read(&resolved).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn api_v1(state: AppState) -> Router<AppState> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/agents", agents::router())
        .nest("/conversations", conversations::router())
        .nest("/llm", llm::router())
        .nest("/integrations", integrations::router())
        .nest("/tasks", tasks::router())
        .nest("/task-runs", task_runs::router())
        .nest("/task-plans", task_plans::router())
        .nest("/approvals", approvals::router())
        .nest("/skills", skills::router())
        .nest("/memories", misc::memories_router())
        .nest("/notifications", misc::notifications_router())
        .nest("/workspace", misc::workspace_router())
        .nest("/audit-events", misc::audit_router())
        .route("/system/status", get(misc::system_status))
        .route("/events/ticket", post(events::issue_ticket))
        .route("/events", get(events::events_ws))
        .with_state(state)
}