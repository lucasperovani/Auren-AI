//! End-to-end REST integration tests: setup → login → agent → task → run →
//! worker execution, plus authorization checks (spec section 3).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::{api, state::AppState};

async fn app() -> (axum::Router, AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = crate::db::connect(&url).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    let skills_dir = dir.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let state = AppState {
        db: pool,
        config: crate::config::AppConfig {
            database_url: url,
            bind_addr: "127.0.0.1:0".into(),
            skills_dir: skills_dir.to_string_lossy().to_string(),
            max_upload_bytes: 1024 * 1024,
            session_secret: "test-secret".into(),
        },
        events: std::sync::Arc::new(crate::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
    };
    (api::router(state.clone()), state, dir)
}

async fn json_response(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn post_json(app: &axum::Router, uri: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.header("content-type", "application/json").body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    json_response(response).await
}

async fn get_json(app: &axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app.clone().oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
    json_response(response).await
}

/// Setup admin and log in; returns the session token.
async fn setup_and_login(app: &axum::Router) -> String {
    let (status, _) = post_json(
        app,
        "/api/v1/auth/setup",
        None,
        json!({ "email": "admin@example.com", "display_name": "Admin", "password": "supersecret1" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = post_json(
        app,
        "/api/v1/auth/login",
        None,
        json!({ "email": "admin@example.com", "password": "supersecret1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["session_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn protected_endpoints_require_auth() {
    let (app, _s, _d) = app().await;
    let (status, _) = get_json(&app, "/api/v1/agents", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get_json(&app, "/api/v1/tasks", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get_json(&app, "/api/v1/system/status", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn full_task_lifecycle() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;

    // Create an agent.
    let (status, agent) = post_json(
        &app,
        "/api/v1/agents",
        Some(&token),
        json!({ "name": "Assistant", "instructions": "Be helpful" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Create a task.
    let (status, task) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "Daily report", "objective": "Summarize inbox" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(task["status"], "draft");
    let task_id = task["id"].as_str().unwrap().to_string();

    // Request a run.
    let (status, run) = post_json(&app, &format!("/api/v1/tasks/{task_id}/run"), Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = run["task_run_id"].as_str().unwrap().to_string();

    // The queue worker executes the run.
    crate::services::worker::tick_once(&state, "test-worker").await.unwrap();
    let (status, run_body) = get_json(&app, &format!("/api/v1/task-runs/{run_id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run_body["status"], "completed");
    assert!(run_body["result"].is_object());

    // Steps were recorded.
    let (status, steps) = get_json(&app, &format!("/api/v1/task-runs/{run_id}/steps"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(steps["items"].as_array().unwrap().len(), 1);
    assert_eq!(steps["items"][0]["status"], "completed");
}

#[tokio::test]
async fn ownership_is_enforced_across_resources() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;

    // A second user cannot exist in single-admin mode, so verify that
    // unknown identifiers return 404 rather than leaking data.
    let (status, _) = get_json(&app, "/api/v1/tasks/nonexistent", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(&app, "/api/v1/task-runs/nonexistent", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(&app, "/api/v1/agents/nonexistent", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_plan_requires_clarifications_before_confirm() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Create a plan.
    let (status, plan) = post_json(
        &app,
        "/api/v1/task-plans",
        Some(&token),
        json!({ "agent_id": agent_id, "request": "Check my email every morning" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();
    assert_eq!(plan["status"], "proposed");
    assert!(!plan["clarifications"].as_array().unwrap().is_empty());

    // Confirming with unanswered clarifications fails.
    let (status, _) = post_json(&app, &format!("/api/v1/task-plans/{plan_id}/confirm"), Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Answer the clarifications.
    let (status, _) = post_json(
        &app,
        &format!("/api/v1/task-plans/{plan_id}/clarifications"),
        Some(&token),
        json!({ "answers": [{ "question": "When should this task run, and how often?", "answer": "Every day at 8am" }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Now confirmation succeeds and creates the task.
    let (status, confirmed) = post_json(&app, &format!("/api/v1/task-plans/{plan_id}/confirm"), Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(confirmed["task_id"].is_string());
}

#[tokio::test]
async fn approval_flow_end_to_end() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;

    // Create a pending approval directly through the service.
    let svc = crate::services::approvals::ApprovalService::new(&state);
    let operation = json!({ "tool": "gmail.send", "args": { "to": "x@y.com" } });
    let approval_id = svc.create(None, None, &operation).await.unwrap();

    // List shows it pending.
    let (status, list) = get_json(&app, "/api/v1/approvals?status=pending", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // Approve with the exact operation.
    let (status, _) = post_json(
        &app,
        &format!("/api/v1/approvals/{approval_id}/approve"),
        Some(&token),
        json!({ "operation": operation }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Revalidation passes for the same operation.
    svc.revalidate(&approval_id, &operation).await.unwrap();
    // And fails for a changed operation.
    let changed = json!({ "tool": "gmail.send", "args": { "to": "other@y.com" } });
    assert!(svc.revalidate(&approval_id, &changed).await.is_err());
}

#[tokio::test]
async fn scheduler_fires_due_schedules() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Create a task with a schedule that fires every minute.
    let (status, task) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "T", "objective": "O", "schedule": { "cron": "* * * * *" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let task_id = task["id"].as_str().unwrap().to_string();

    // Force the schedule due and tick.
    sqlx::query("UPDATE scheduled_jobs SET next_run_at = '2020-01-01T00:00:00Z' WHERE task_id = ?")
        .bind(&task_id)
        .execute(&state.db)
        .await
        .unwrap();
    let fired = crate::services::scheduler::tick(&state.db).await.unwrap();
    assert_eq!(fired, 1);

    // A run was created and queued.
    let (status, runs) = get_json(&app, &format!("/api/v1/tasks/{task_id}/runs"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs["items"].as_array().unwrap().len(), 1);
    assert_eq!(runs["items"][0]["status"], "queued");

    // next_run_at advanced into the future.
    let next: String = sqlx::query("SELECT next_run_at FROM scheduled_jobs WHERE task_id = ?")
        .bind(&task_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .get("next_run_at");
    assert!(next.as_str() > "2026-01-01");
}

use sqlx::Row;

#[tokio::test]
async fn workspace_rejects_traversal() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;

    let (status, _) = get_json(&app, "/api/v1/workspace/files/content?path=../secrets.txt", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = get_json(&app, "/api/v1/workspace/files/content?path=/etc/passwd", Some(&token)).await;
    assert_neq_not_found_or_forbidden(status);
}

fn assert_neq_not_found_or_forbidden(status: StatusCode) {
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn notifications_lifecycle() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;

    // Resolve the real user id from the session.
    let user = crate::api::auth_user::AuthUser::from_token(&state, &token).await.unwrap();

    // Create a notification through the service.
    let svc = crate::services::notifications::NotificationService::new(&state);
    let notification_id = svc.create(&user.id, "task_failed", "Task failed", "Boom", None, None, None).await.unwrap();

    let (status, list) = get_json(&app, "/api/v1/notifications", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    let (status, _) = post_json(&app, &format!("/api/v1/notifications/{notification_id}/read"), Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = get_json(&app, &format!("/api/v1/notifications/{notification_id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["read_at"].is_string());
}