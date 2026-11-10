//! Network-dependent checks for web research.
//!
//! These are `#[ignore]`d so the default suite stays hermetic; run them with
//! `cargo test -p auren-server -- --ignored web_research_live`.

use crate::services::web_research;
use crate::state::AppState;

async fn state() -> AppState {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = crate::db::connect(&url).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    std::mem::forget(dir);
    AppState {
        db: pool,
        config: crate::config::AppConfig {
            database_url: url,
            bind_addr: "127.0.0.1:0".into(),
            skills_dir: "data/skills".into(),
            max_upload_bytes: 1024,
            session_secret: "s".into(),
            workspace_dir: "data/workspace".into(),
            max_workspace_file_bytes: 1024,
            credential_encryption_key: "k".into(),
            max_iterations: 25,
            max_tool_calls: 50,
            max_llm_calls: 40,
            subagent_auto_delete: false,
        },
        events: std::sync::Arc::new(crate::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
        rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
    }
}

/// A real search returns at least one result with a usable URL.
#[tokio::test]
#[ignore = "requires network access"]
async fn web_research_live_search() {
    let state = state().await;
    let results = web_research::search(&state, "rust programming language", 5)
        .await
        .expect("search should not fail");
    assert!(!results.is_empty(), "expected results for a common query");
    for r in &results {
        assert!(!r.title.is_empty());
        assert!(r.url.starts_with("http"), "unwrapped url: {}", r.url);
        // The redirect wrapper must never leak to the caller.
        assert!(!r.url.contains("duckduckgo.com/l/"), "wrapper leaked: {}", r.url);
    }
}

/// A real page fetch returns readable text, not markup.
#[tokio::test]
#[ignore = "requires network access"]
async fn web_research_live_fetch() {
    let state = state().await;
    let page = web_research::fetch_page(&state, "https://example.com/")
        .await
        .expect("fetch should not fail");
    assert_eq!(page["status"], 200);
    let text = page["text"].as_str().unwrap();
    // example.com is a placeholder domain; assert on structure, not wording.
    assert!(text.len() > 40, "expected substantial text, got: {text}");
    assert!(
        text.to_ascii_lowercase().contains("domain"),
        "expected page prose, got: {text}"
    );
    // Markup must not survive the conversion.
    assert!(!text.contains("<html"), "raw markup leaked: {text}");
    assert!(!text.contains("<p>"), "raw markup leaked: {text}");
    assert!(page["title"].as_str().is_some_and(|t| !t.is_empty()));
}

/// Internal destinations are refused even when reached via a redirect.
#[tokio::test]
#[ignore = "requires network access"]
async fn web_research_live_blocks_internal() {
    let state = state().await;
    let err = web_research::fetch_page(&state, "http://169.254.169.254/latest/meta-data/")
        .await
        .expect_err("metadata endpoint must be refused");
    assert!(err.to_string().contains("internal") || err.to_string().contains("forbidden"));
}
