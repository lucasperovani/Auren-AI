//! Live navigation check: requires a Chromium-compatible browser on the host.
//!
//! Run with: `cargo test -p auren-server --lib -- --ignored browser_live`

use crate::services::browser;

/// A JavaScript-rendered page must yield text that plain HTML fetching cannot
/// see. `example.com` has no scripts, so the assertion is on rendering plumbing
/// rather than on a specific site's content.
#[tokio::test]
#[ignore = "requires a headless browser on the host"]
async fn browser_live_renders_a_page() {
    if !browser::is_available() {
        eprintln!("no browser found; skipping");
        return;
    }
    let page = browser::navigate("https://example.com/")
        .await
        .expect("navigation should succeed");
    assert_eq!(page["rendered"], true);
    let text = page["text"].as_str().unwrap();
    assert!(text.len() > 40, "expected rendered text, got: {text}");
    assert!(page["title"].as_str().is_some_and(|t| !t.is_empty()));
    assert!(!text.contains("<html"), "markup leaked: {text}");
}

/// Navigation refuses internal destinations before starting the browser.
#[tokio::test]
#[ignore = "requires a headless browser on the host"]
async fn browser_live_blocks_internal_urls() {
    let err = browser::navigate("http://127.0.0.1:8080/")
        .await
        .expect_err("loopback must be refused");
    assert!(err.to_string().contains("internal") || err.to_string().contains("forbidden"));
}
