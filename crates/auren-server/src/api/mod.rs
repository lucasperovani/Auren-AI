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
pub mod skills;
pub mod task_plans;
pub mod task_runs;
pub mod tasks;

#[cfg(test)]
mod events_tests;
#[cfg(test)]
mod integration_tests;

use axum::{routing::{get, post}, Router};

use crate::state::AppState;

/// Build the `/api/v1` router. Resource routers are added here as they are
/// implemented; see docs/api/openapi.yaml for the full contract.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(health::liveness))
        .route("/health/ready", get(health::readiness))
        .nest("/api/v1", api_v1(state.clone()))
        .with_state(state)
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