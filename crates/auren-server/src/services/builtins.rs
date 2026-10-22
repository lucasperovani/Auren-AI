//! Built-in skills (spec section 10): File Management and Web Research.
//!
//! These are registered as declarative builtin implementations that the
//! ToolRegistry can execute. They enforce the same permission engine and
//! workspace restrictions as installed skills.

use serde_json::{json, Value};

use crate::{error::ApiError, state::AppState};

/// Execute a builtin implementation by reference. This is the dispatch
/// point behind `implementation_ref` in declarative tools.
pub async fn execute_builtin(
    state: &AppState,
    reference: &str,
    args: &Value,
) -> Result<Value, ApiError> {
    match reference {
        "builtin.echo" => Ok(args.clone()),
        "builtin.noop" => Ok(json!({ "ok": true })),
        "builtin.file_list" => file_list(state, args).await,
        "builtin.file_read" => file_read(state, args).await,
        "builtin.file_write" => file_write(state, args).await,
        "builtin.file_delete" => file_delete(state, args).await,
        "builtin.file_search" => file_search(state, args).await,
        "builtin.web_fetch" => web_fetch(state, args).await,
        other => Err(ApiError::Unprocessable(format!("unknown builtin '{other}'"))),
    }
}

/// Resolve a path inside the authorized workspace, rejecting traversal
/// (spec section 4: workspace security).
fn resolve_workspace_path(state: &AppState, path: &str) -> Result<std::path::PathBuf, ApiError> {
    let root = std::path::PathBuf::from(&state.config.workspace_dir);
    if path.starts_with('/') {
        return Err(ApiError::Forbidden("absolute paths are not allowed".into()));
    }
    let relative = path.trim_start_matches('/');
    if relative.is_empty() {
        return Err(ApiError::Unprocessable("path is required".into()));
    }
    for component in root.join(relative).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ApiError::Forbidden("path traversal is not allowed".into()));
        }
    }
    let resolved = root.join(relative);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
    if !canonical.starts_with(&canonical_root) {
        return Err(ApiError::Forbidden("path is outside the authorized workspace".into()));
    }
    Ok(canonical)
}

async fn file_list(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let root = std::path::PathBuf::from(&state.config.workspace_dir);
    let dir = if path.is_empty() { root.clone() } else { resolve_workspace_path(state, path)? };
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            items.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "type": if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { "directory" } else { "file" },
                "size": entry.metadata().map(|m| m.len()).unwrap_or(0),
            }));
        }
    }
    Ok(json!({ "items": items }))
}

async fn file_read(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let resolved = resolve_workspace_path(state, path)?;
    let bytes = std::fs::read(&resolved).map_err(|_| ApiError::NotFound)?;
    if bytes.len() as u64 > state.config.max_workspace_file_bytes {
        return Err(ApiError::ContentTooLarge);
    }
    Ok(json!({ "path": path, "content": String::from_utf8_lossy(&bytes) }))
}

async fn file_write(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or_default();
    if content.len() as u64 > state.config.max_workspace_file_bytes {
        return Err(ApiError::ContentTooLarge);
    }
    let resolved = resolve_workspace_path(state, path)?;
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    }
    std::fs::write(&resolved, content).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    Ok(json!({ "path": path, "bytes_written": content.len() }))
}

/// Destructive operation: recorded for the approval engine when policy
/// requires it (the permission engine decides based on risk class).
async fn file_delete(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let resolved = resolve_workspace_path(state, path)?;
    let result = if resolved.is_dir() {
        std::fs::remove_dir(&resolved)
    } else {
        std::fs::remove_file(&resolved)
    };
    result.map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    Ok(json!({ "path": path, "deleted": true }))
}

async fn file_search(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("query is required".into()))?;
    let root = std::path::PathBuf::from(&state.config.workspace_dir);
    let mut matches = Vec::new();
    search_recursive(&root, query, &mut matches, 0);
    Ok(json!({ "matches": matches }))
}

fn search_recursive(dir: &std::path::Path, query: &str, matches: &mut Vec<Value>, depth: u32) {
    if depth > 8 || matches.len() >= 100 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                search_recursive(&path, query, matches, depth + 1);
            } else if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains(query) {
                    matches.push(json!({
                        "path": path.strip_prefix(dir).unwrap_or(&path).to_string_lossy(),
                        "line": content.lines().position(|l| l.contains(query)).map(|i| i + 1),
                    }));
                }
            }
        }
    }
}

/// Web research: fetch a public URL with SSRF protection (spec section 10.2
/// and 15: never reach private/internal destinations).
async fn web_fetch(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("url is required".into()))?;
    validate_public_url(url)?;

    let response = reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| {
            tracing::debug!(error = %e, "web fetch failed");
            ApiError::ServiceUnavailable
        })?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().await.unwrap_or_default();
    // Truncate large bodies; retrieved content is untrusted input.
    let truncated: String = body.chars().take(100_000).collect();
    let _ = state;
    Ok(json!({
        "url": url,
        "status": status,
        "content_type": content_type,
        "content": truncated,
    }))
}

/// Reject non-http(s) URLs and private/internal destinations (SSRF).
pub fn validate_public_url(url: &str) -> Result<(), ApiError> {
    let parsed = url::Url::parse(url).map_err(|_| ApiError::Unprocessable("invalid url".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ApiError::Unprocessable("url must be http(s)".into()));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host.is_empty() || host == "localhost" || host.ends_with(".internal") || host.ends_with(".local") {
        return Err(ApiError::Forbidden("access to internal hosts is not allowed".into()));
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let internal = match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified() || v4.is_private(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
        if internal {
            return Err(ApiError::Forbidden("access to internal addresses is not allowed".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    async fn state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let pool = crate::db::connect("sqlite::memory:").await.unwrap();
        let s = AppState {
            db: pool,
            config: crate::config::AppConfig {
                database_url: "sqlite::memory:".into(),
                bind_addr: "127.0.0.1:0".into(),
                skills_dir: "data/skills".into(),
                max_upload_bytes: 1024,
                session_secret: "s".into(),
                workspace_dir: workspace.to_string_lossy().to_string(),
                max_workspace_file_bytes: 1024 * 1024,
                credential_encryption_key: "k".into(),
            },
            events: std::sync::Arc::new(crate::events::EventBus::new()),
            ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
        };
        (s, dir)
    }

    #[tokio::test]
    async fn file_operations_roundtrip() {
        let (s, _d) = state().await;
        let written = execute_builtin(&s, "builtin.file_write", &json!({ "path": "notes/a.txt", "content": "hello world" })).await.unwrap();
        assert_eq!(written["bytes_written"], 11);
        let read = execute_builtin(&s, "builtin.file_read", &json!({ "path": "notes/a.txt" })).await.unwrap();
        assert_eq!(read["content"], "hello world");
        let list = execute_builtin(&s, "builtin.file_list", &json!({ "path": "notes" })).await.unwrap();
        assert_eq!(list["items"].as_array().unwrap().len(), 1);
        let search = execute_builtin(&s, "builtin.file_search", &json!({ "query": "hello" })).await.unwrap();
        assert_eq!(search["matches"].as_array().unwrap().len(), 1);
        execute_builtin(&s, "builtin.file_delete", &json!({ "path": "notes/a.txt" })).await.unwrap();
        assert!(execute_builtin(&s, "builtin.file_read", &json!({ "path": "notes/a.txt" })).await.is_err());
    }

    #[tokio::test]
    async fn workspace_traversal_rejected() {
        let (s, _d) = state().await;
        assert!(execute_builtin(&s, "builtin.file_read", &json!({ "path": "../secrets" })).await.is_err());
        assert!(execute_builtin(&s, "builtin.file_write", &json!({ "path": "/etc/evil", "content": "x" })).await.is_err());
    }

    #[test]
    fn ssrf_protection() {
        assert!(validate_public_url("https://example.com").is_ok());
        assert!(validate_public_url("http://localhost/x").is_err());
        assert!(validate_public_url("http://127.0.0.1/x").is_err());
        assert!(validate_public_url("http://10.0.0.1/x").is_err());
        assert!(validate_public_url("http://192.168.1.1/x").is_err());
        assert!(validate_public_url("file:///etc/passwd").is_err());
        assert!(validate_public_url("http://service.internal/x").is_err());
    }
}