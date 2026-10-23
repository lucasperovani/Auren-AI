//! End-to-end REST integration tests: setup → login → agent → task → run →
//! worker execution, plus authorization checks (spec section 3).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use sqlx::Row;
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
            workspace_dir: "data/workspace".into(),
            max_workspace_file_bytes: 1024 * 1024,
            credential_encryption_key: "k".into(),
                max_iterations: 25,
                max_tool_calls: 50,
                max_llm_calls: 40,
                subagent_auto_delete: false,
        },
        events: std::sync::Arc::new(crate::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
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

async fn put_json(app: &axum::Router, uri: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("PUT").uri(uri);
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

    // Without an LLM provider configured, the runtime records an honest
    // failure instead of faking success (spec section 21). The first
    // failure schedules a retry, so exhaust the attempts to reach the
    // terminal failed state.
    for _ in 0..5 {
        sqlx::query("UPDATE queued_jobs SET available_at = ? WHERE task_run_id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&run_id)
            .execute(&state.db)
            .await
            .unwrap();
        crate::services::worker::tick_once(&state, "test-worker").await.unwrap();
    }
    let (status, run_body) = get_json(&app, &format!("/api/v1/task-runs/{run_id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run_body["status"], "failed");
    assert!(run_body["error_message"]
        .as_str()
        .unwrap()
        .contains("no LLM provider configured"));

    // With a mock LLM provider configured, the same run completes.
    let mock_base = start_mock_llm().await;
    sqlx::query("INSERT INTO llm_providers (id, user_id, name, provider_type, base_url, status, created_at, updated_at) VALUES ('p1', ?, 'Mock', 'openai_compatible', ?, 'active', '2026', '2026')")
        .bind(sqlx::query("SELECT user_id FROM tasks WHERE id = ?").bind(&task_id).fetch_one(&state.db).await.unwrap().try_get::<String, _>("user_id").unwrap())
        .bind(&mock_base)
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO llm_models (id, provider_id, model_name, capabilities_json, created_at) VALUES ('m1', 'p1', 'mock-model', '{}', '2026')")
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE agents SET llm_provider_id = 'p1', model_id = 'm1' WHERE id = ?")
        .bind(&agent_id)
        .execute(&state.db)
        .await
        .unwrap();

    // Retry the failed run.
    sqlx::query("UPDATE queued_jobs SET status = 'pending', available_at = ? WHERE task_run_id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&run_id)
        .execute(&state.db)
        .await
        .unwrap();
    crate::services::worker::tick_once(&state, "test-worker").await.unwrap();
    let (status, run_body) = get_json(&app, &format!("/api/v1/task-runs/{run_id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run_body["status"], "completed");
    assert!(run_body["result"].is_object());
}

/// Start a mock OpenAI-compatible server that answers a fixed completion.
async fn start_mock_llm() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let service = hyper::service::service_fn(|_req| async {
                let body = serde_json::json!({
                    "choices": [{
                        "message": { "content": "Task acknowledged: objective received.", "finish_reason": "stop" }
                    }],
                    "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
                });
                Ok::<_, std::convert::Infallible>(hyper::Response::new(axum::body::Body::from(body.to_string())))
            });
            let http = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
            let _ = http.serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service).await;
        }
    });
    format!("http://{addr}")
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
async fn chat_approval_is_visible_and_decidable_by_conversation_owner() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;

    // Agent and conversation owned by the admin user.
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let (status, conversation) = post_json(
        &app,
        "/api/v1/conversations",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "t" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let conversation_id = conversation["id"].as_str().unwrap().to_string();

    // A chat approval bound to the conversation (as the runtime creates it).
    let svc = crate::services::approvals::ApprovalService::new(&state);
    let operation = json!({ "agent_id": agent_id, "tool_name": "git_push", "args": {} });
    let approval_id = svc
        .create_with_conversation(None, None, Some(&conversation_id), &operation)
        .await
        .unwrap();

    // The pending list shows the tool name and arguments.
    let (status, list) = get_json(&app, "/api/v1/approvals?status=pending", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let item = list["items"].as_array().unwrap().iter().find(|i| i["id"] == approval_id).unwrap().clone();
    assert_eq!(item["tool_name"], "git_push");
    assert_eq!(item["conversation_id"], conversation_id);

    // The conversation owner can read the full operation.
    let (status, detail) = get_json(&app, &format!("/api/v1/approvals/{approval_id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["operation"]["tool_name"], "git_push");

    // And decide it inline.
    let (status, _) = post_json(
        &app,
        &format!("/api/v1/approvals/{approval_id}/approve"),
        Some(&token),
        json!({ "operation": operation }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    svc.revalidate(&approval_id, &operation).await.unwrap();
}

#[tokio::test]
async fn auto_approve_settings_roundtrip() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;

    // Default is off.
    let (_, body) = get_json(&app, "/api/v1/workspace/auto-approve", Some(&token)).await;
    assert_eq!(body["enabled"], false);

    // Enable globally.
    let (status, _) = put_json(&app, "/api/v1/workspace/auto-approve", Some(&token), json!({ "enabled": true })).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_json(&app, "/api/v1/workspace/auto-approve", Some(&token)).await;
    assert_eq!(body["enabled"], true);

    // Per-conversation override.
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let (_, conversation) = post_json(
        &app,
        "/api/v1/conversations",
        Some(&token),
        json!({ "agent_id": agent["id"], "title": "t" }),
    )
    .await;
    let conversation_id = conversation["id"].as_str().unwrap();
    let (status, _) = put_json(
        &app,
        &format!("/api/v1/conversations/{conversation_id}/auto-approve"),
        Some(&token),
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_json(&app, &format!("/api/v1/conversations/{conversation_id}/auto-approve"), Some(&token)).await;
    assert_eq!(body["enabled"], false);
    // Clearing the override returns null (follow global).
    let (status, _) = put_json(
        &app,
        &format!("/api/v1/conversations/{conversation_id}/auto-approve"),
        Some(&token),
        json!({ "enabled": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = get_json(&app, &format!("/api/v1/conversations/{conversation_id}/auto-approve"), Some(&token)).await;
    assert!(body["enabled"].is_null());
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