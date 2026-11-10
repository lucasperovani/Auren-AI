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

/// Notifications carry a severity and action buttons, the user can pick an
/// action, and deleting removes it from the inbox.
#[tokio::test]
async fn notification_actions_roundtrip() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;
    let user = crate::api::auth_user::AuthUser::from_token(&state, &token).await.unwrap();

    let svc = crate::services::notifications::NotificationService::new(&state);
    let id = svc
        .create_with_actions(
            &user.id,
            "rate_limit",
            "Limit reached",
            "Queued the operation.",
            crate::services::notifications::NotificationOptions {
                severity: "warning",
                actions: vec![crate::services::notifications::NotificationAction {
                    id: "dismiss_it".into(),
                    label: "Dismiss".into(),
                    style: String::new(),
                    kind: "dismiss".into(),
                    payload: json!({}),
                }],
            },
        )
        .await
        .unwrap();

    // The list exposes severity and actions.
    let (status, list) = get_json(&app, "/api/v1/notifications?limit=10", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let item = list["items"].as_array().unwrap().iter().find(|i| i["id"] == id).unwrap().clone();
    assert_eq!(item["severity"], "warning");
    assert_eq!(item["actions"][0]["id"], "dismiss_it");
    assert!(item["action_id"].is_null());

    // Unread count reflects the new notification.
    let (status, count) = get_json(&app, "/api/v1/notifications/unread-count", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count["unread"], 1);

    // Choosing an action records it and marks the notification read.
    let (status, body) = post_json(
        &app,
        &format!("/api/v1/notifications/{id}/actions/dismiss_it"),
        Some(&token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "dismissed");
    let (_, detail) = get_json(&app, &format!("/api/v1/notifications/{id}"), Some(&token)).await;
    assert_eq!(detail["action_id"], "dismiss_it");
    assert!(detail["acted_at"].is_string());
    assert!(detail["read_at"].is_string());

    // An unknown action is rejected, never silently accepted.
    let (status, _) = post_json(
        &app,
        &format!("/api/v1/notifications/{id}/actions/nope"),
        Some(&token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Deleting removes it from the inbox.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/notifications/{id}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (status, _) = get_json(&app, &format!("/api/v1/notifications/{id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A metered operation whose quota is exhausted is deferred, not failed, and
/// the user is notified once. The worker only runs it after the window opens.
#[tokio::test]
async fn exhausted_quota_defers_instead_of_failing() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;
    let user = crate::api::auth_user::AuthUser::from_token(&state, &token).await.unwrap();

    // A connected Instagram account with a stored token.
    sqlx::query("INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at) VALUES ('i1', ?, 'instagram', 'IG', 'connected', '2026', '2026')")
        .bind(&user.id)
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, status, created_at, updated_at) VALUES ('a1', 'i1', 'ext', 'IG', 'active', '2026', '2026')")
        .execute(&state.db)
        .await
        .unwrap();

    let policy = crate::services::rate_limits::INSTAGRAM_PUBLISH;
    // Fill the quota.
    for _ in 0..policy.limit {
        crate::services::rate_limits::consume(&state, "a1", policy).await.unwrap();
    }
    let status_before = crate::services::rate_limits::status(&state, "a1", policy).await.unwrap();
    assert!(status_before.exhausted());
    assert_eq!(status_before.remaining(), 0);

    // Deferring records the operation and notifies the user.
    let deferred_id = crate::services::rate_limits::defer(
        &state, "a1", &user.id, "instagram", "publish_media",
        &json!({ "image_url": "https://example.com/a.jpg" }),
        &status_before,
        "The publishing limit was reached.",
    )
    .await
    .unwrap();

    // Repeating the same operation does not queue a duplicate.
    let again = crate::services::rate_limits::defer(
        &state, "a1", &user.id, "instagram", "publish_media",
        &json!({ "image_url": "https://example.com/a.jpg" }),
        &status_before,
        "The publishing limit was reached.",
    )
    .await
    .unwrap();
    assert_eq!(deferred_id, again);
    let queued: i64 = sqlx::query("SELECT COUNT(*) AS n FROM deferred_operations WHERE status = 'pending'")
        .fetch_one(&state.db).await.unwrap().get("n");
    assert_eq!(queued, 1);

    // The notification explains the wait and offers a next step.
    let (_, list) = get_json(&app, "/api/v1/notifications?limit=10", Some(&token)).await;
    let notification = list["items"].as_array().unwrap().iter()
        .find(|n| n["notification_type"] == "rate_limit")
        .expect("rate-limit notification created");
    assert_eq!(notification["severity"], "warning");
    assert_eq!(notification["actions"][0]["kind"], "agent");

    // The operation is scheduled in the future, so no worker picks it up now.
    let due = crate::services::rate_limits::due_operations(&state, 10).await.unwrap();
    assert!(due.is_empty(), "deferred work must not run before the window opens");

    // The UI can read the account's quota and queue.
    let (status, usage) = get_json(&app, "/api/v1/integrations/i1/accounts/a1/rate-limits", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let publish = usage["limits"].as_array().unwrap().iter()
        .find(|l| l["bucket"] == "publish").expect("publish bucket reported");
    assert_eq!(publish["limit"], 50);
    assert_eq!(publish["remaining"], 0);
    assert_eq!(usage["deferred"].as_array().unwrap().len(), 1);
    assert_eq!(usage["deferred"][0]["operation"], "publish_media");
}

/// Deferring is only used for metered providers; a successful call consumes
/// quota that a later call cannot exceed.
#[tokio::test]
async fn quota_rolls_over_after_the_window() {
    let (_app, state, _d) = app().await;
    // Start with an already-expired window: usage must read as zero.
    sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
        .execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at) VALUES ('i1', 'u1', 'instagram', 'IG', 'connected', '2026', '2026')")
        .execute(&state.db).await.unwrap();
    sqlx::query("INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, status, created_at, updated_at) VALUES ('a1', 'i1', 'ext', 'IG', 'active', '2026', '2026')")
        .execute(&state.db).await.unwrap();

    let policy = crate::services::rate_limits::INSTAGRAM_PUBLISH;
    let long_ago = (chrono::Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    sqlx::query(
        "INSERT INTO rate_limit_usage (id, integration_account_id, bucket, window_seconds, used, limit_value, window_started_at, updated_at)
         VALUES ('r1', 'a1', 'publish', ?, 50, 50, ?, ?)",
    )
    .bind(policy.window_seconds)
    .bind(&long_ago)
    .bind(&long_ago)
    .execute(&state.db).await.unwrap();

    let status = crate::services::rate_limits::status(&state, "a1", policy).await.unwrap();
    assert_eq!(status.used, 0, "an expired window must not carry usage over");
    assert!(!status.exhausted());
}

/// The task payload carries everything the UI needs to describe a schedule in
/// plain language: the cron, the next fire time, and the latest run.
#[tokio::test]
async fn task_payload_exposes_schedule_and_last_run() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();

    let (status, scheduled) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "T", "objective": "O", "schedule": { "cron": "0 9 * * 1-5" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(scheduled["schedule_cron"], "0 9 * * 1-5");
    assert!(scheduled["next_run_at"].is_string());
    assert!(scheduled["last_run_at"].is_null());

    let (status, manual) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "M", "objective": "O" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(manual["schedule_cron"].is_null());
    assert!(manual["next_run_at"].is_null());

    // Listing joins the same fields, so the board needs no extra requests.
    let (status, list) = get_json(&app, "/api/v1/tasks?limit=10", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let item = list["items"].as_array().unwrap().iter()
        .find(|t| t["title"] == "T").expect("scheduled task listed");
    assert_eq!(item["schedule_cron"], "0 9 * * 1-5");
    assert!(item["next_run_at"].is_string());
}

/// PATCH with an explicit `null` schedule removes the schedule; omitting the
/// field leaves the existing schedule untouched.
#[tokio::test]
async fn task_schedule_can_be_removed_or_left_untouched() {
    let (app, _s, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let (_, task) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "T", "objective": "O", "schedule": { "cron": "0 9 * * *" } }),
    )
    .await;
    let task_id = task["id"].as_str().unwrap().to_string();
    assert_eq!(task["status"], "active");

    // Renaming without a schedule field keeps the schedule.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/tasks/{task_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "title": "Renamed" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = json_response(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "Renamed");
    assert_eq!(body["schedule_cron"], "0 9 * * *");
    assert!(body["next_run_at"].is_string());

    // An explicit null clears it and drops the task to on-demand.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/tasks/{task_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "schedule": null }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = json_response(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["schedule_cron"].is_null());
    assert!(body["next_run_at"].is_null());
    assert_eq!(body["status"], "draft");
}

/// Tasks land in the right board column on creation, and moving a card never
/// changes the execution status that drives the scheduler.
#[tokio::test]
async fn kanban_stage_is_independent_from_execution_status() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // A scheduled task is ready to run, so it starts in To Do.
    let (status, scheduled) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "Scheduled", "objective": "O", "schedule": { "cron": "0 9 * * *" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(scheduled["stage"], "todo");
    let scheduled_id = scheduled["id"].as_str().unwrap().to_string();

    // An on-demand task still needs scope, so it starts in Analysis.
    let (_, manual) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "Manual", "objective": "O" }),
    )
    .await;
    assert_eq!(manual["stage"], "analysis");

    // Moving to another column changes only the stage.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{scheduled_id}/stage"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "stage": "in_progress" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, moved) = json_response(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["stage"], "in_progress");
    // The schedule is untouched: the task still fires.
    assert_eq!(moved["status"], "active");
    assert_eq!(moved["schedule_cron"], "0 9 * * *");
    let next: String = sqlx::query("SELECT next_run_at FROM scheduled_jobs WHERE task_id = ?")
        .bind(&scheduled_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .get("next_run_at");
    assert!(!next.is_empty());

    // An unknown column is rejected instead of being stored blindly.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{scheduled_id}/stage"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "stage": "wishlist" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Dragging is also possible through the regular PATCH.
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/tasks/{scheduled_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "stage": "todo" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = json_response(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stage"], "todo");
    assert_eq!(body["status"], "active");
}

/// The board advances automatically: a started run moves the card to In
/// Progress and a finished one moves it to Done, without touching `status`.
#[tokio::test]
async fn kanban_advances_with_run_lifecycle() {
    let (app, state, _d) = app().await;
    let token = setup_and_login(&app).await;
    let (_, agent) = post_json(&app, "/api/v1/agents", Some(&token), json!({ "name": "A" })).await;
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let (_, task) = post_json(
        &app,
        "/api/v1/tasks",
        Some(&token),
        json!({ "agent_id": agent_id, "title": "T", "objective": "O" }),
    )
    .await;
    let task_id = task["id"].as_str().unwrap().to_string();
    assert_eq!(task["stage"], "analysis");

    // Queue and execute one run. No LLM provider is configured, so the run
    // ends failed — the stage must still reflect the lifecycle.
    let (_, run) = post_json(&app, &format!("/api/v1/tasks/{task_id}/run"), Some(&token), json!({})).await;
    let run_id = run["task_run_id"].as_str().unwrap().to_string();
    crate::services::worker::tick_once(&state, "test-worker").await.unwrap();

    let stage: String = sqlx::query("SELECT stage FROM tasks WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .get("stage");
    assert_eq!(stage, "in_progress", "a started run moves the card forward");

    // A completed run closes the card.
    sqlx::query("UPDATE queued_jobs SET status = 'done' WHERE task_run_id = ?")
        .bind(&run_id)
        .execute(&state.db)
        .await
        .unwrap();
    crate::services::scheduler::complete_job(&state.db, "unused-job", &run_id, "{}")
        .await
        .unwrap();
    let stage: String = sqlx::query("SELECT stage FROM tasks WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .get("stage");
    assert_eq!(stage, "done", "a completed run closes the card");
    // Execution status is untouched by the board transition.
    let status: String = sqlx::query("SELECT status FROM tasks WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&state.db)
        .await
        .unwrap()
        .get("status");
    assert_eq!(status, "draft");
}