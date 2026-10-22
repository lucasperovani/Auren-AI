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
        // Code editing tools (Copilot-style, operating on the workspace).
        "builtin.code_read_file" => code_read_file(state, args).await,
        "builtin.code_write_file" => code_write_file(state, args).await,
        "builtin.code_edit_block" => code_edit_block(state, args).await,
        "builtin.code_list_dir" => code_list_dir(state, args).await,
        "builtin.code_search" => code_search(state, args).await,
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
    // The root must exist (created at startup); canonicalize it once so the
    // containment check works for files that do not exist yet.
    let canonical_root = root
        .canonicalize()
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("workspace root unavailable: {e}")))?;
    let resolved = root.join(relative);
    // Canonicalize the deepest existing ancestor and append the remainder:
    // this resolves symlinks where present while supporting new files.
    let mut existing = resolved.clone();
    let mut suffix = Vec::new();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => {
                suffix.push(existing.file_name().map(|n| n.to_os_string()).unwrap_or_default());
                existing = parent.to_path_buf();
            }
            None => break,
        }
    }
    let mut canonical = existing.canonicalize().unwrap_or_else(|_| existing.clone());
    for part in suffix.iter().rev() {
        canonical = canonical.join(part);
    }
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

// ---------------------------------------------------------------------------
// Code editing tools (Copilot-style, operating on the workspace).
// These give the agent the ability to read, create, and surgically edit
// code files in the workspace. Nothing is executed automatically (spec
// part1 section 4: code execution is a separately permissioned capability).
// ---------------------------------------------------------------------------

/// Read a code file with line numbers, optionally a line range.
async fn code_read_file(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let resolved = resolve_workspace_path(state, path)?;
    let content = std::fs::read_to_string(&resolved).map_err(|_| ApiError::NotFound)?;
    let start = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
    let end = args.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(usize::MAX);
    let lines: Vec<String> = content
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(end.saturating_sub(start - 1))
        .map(|(i, l)| format!("{:>4} | {}", i + 1, l))
        .collect();
    Ok(json!({
        "path": path,
        "total_lines": content.lines().count(),
        "content": lines.join("\n"),
    }))
}

/// Create or overwrite a code file.
async fn code_write_file(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or_default();
    let resolved = resolve_workspace_path(state, path)?;
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    }
    std::fs::write(&resolved, content).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    Ok(json!({ "path": path, "lines_written": content.lines().count() }))
}

/// Surgical edit: replace an exact text block with new content (Copilot-style
/// find/replace). The `old_text` must match exactly and uniquely; ambiguous
/// matches are rejected so the agent never edits the wrong place.
async fn code_edit_block(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let old_text = args.get("old_text").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("old_text is required".into()))?;
    let new_text = args.get("new_text").and_then(|v| v.as_str()).unwrap_or_default();

    let resolved = resolve_workspace_path(state, path)?;
    let content = std::fs::read_to_string(&resolved).map_err(|_| ApiError::NotFound)?;

    let occurrences = content.matches(old_text).count();
    if occurrences == 0 {
        return Err(ApiError::Unprocessable(
            "old_text not found in the file; read the file first and copy the exact text".into(),
        ));
    }
    if occurrences > 1 {
        return Err(ApiError::Unprocessable(format!(
            "old_text matches {occurrences} locations; include more surrounding lines to make it unique"
        )));
    }
    let updated = content.replacen(old_text, new_text, 1);
    std::fs::write(&resolved, &updated).map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    Ok(json!({ "path": path, "edited": true }))
}

/// List a directory tree (bounded depth) for code exploration.
async fn code_list_dir(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let root = std::path::PathBuf::from(&state.config.workspace_dir);
    let dir = if path.is_empty() { root.clone() } else { resolve_workspace_path(state, path)? };
    let mut items = Vec::new();
    list_tree(&dir, &root, &mut items, 0, 3);
    Ok(json!({ "items": items }))
}

fn list_tree(dir: &std::path::Path, root: &std::path::Path, items: &mut Vec<Value>, depth: u32, max_depth: u32) {
    if depth > max_depth || items.len() >= 200 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
            if path.is_dir() {
                items.push(json!({ "path": relative, "type": "directory" }));
                list_tree(&path, root, items, depth + 1, max_depth);
            } else {
                items.push(json!({
                    "path": relative,
                    "type": "file",
                    "size": entry.metadata().map(|m| m.len()).unwrap_or(0),
                }));
            }
        }
    }
}

/// Search code with a text query across the workspace.
async fn code_search(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let query = args.get("query").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("query is required".into()))?;
    let root = std::path::PathBuf::from(&state.config.workspace_dir);
    let mut matches = Vec::new();
    search_code_recursive(&root, query, &mut matches, 0);
    Ok(json!({ "matches": matches }))
}

fn search_code_recursive(dir: &std::path::Path, query: &str, matches: &mut Vec<Value>, depth: u32) {
    if depth > 8 || matches.len() >= 50 {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                search_code_recursive(&path, query, matches, depth + 1);
            } else if let Ok(content) = std::fs::read_to_string(&path) {
                for (i, line) in content.lines().enumerate() {
                    if line.contains(query) {
                        matches.push(json!({
                            "path": path.to_string_lossy(),
                            "line": i + 1,
                            "text": line.trim().chars().take(200).collect::<String>(),
                        }));
                        if matches.len() >= 50 {
                            return;
                        }
                    }
                }
            }
        }
    }
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

    #[tokio::test]
    async fn code_edit_block_is_surgical() {
        let (s, _d) = state().await;
        execute_builtin(&s, "builtin.code_write_file", &json!({
            "path": "src/main.py",
            "content": "def main():\n    print('hello')\n    return 0\n"
        })).await.unwrap();

        // Unique match: succeeds.
        let edited = execute_builtin(&s, "builtin.code_edit_block", &json!({
            "path": "src/main.py",
            "old_text": "print('hello')",
            "new_text": "print('hello, world')"
        })).await.unwrap();
        assert_eq!(edited["edited"], true);
        let read = execute_builtin(&s, "builtin.code_read_file", &json!({ "path": "src/main.py" })).await.unwrap();
        assert!(read["content"].as_str().unwrap().contains("hello, world"));

        // Ambiguous match: rejected.
        execute_builtin(&s, "builtin.code_write_file", &json!({
            "path": "src/dup.py",
            "content": "x = 1\nx = 1\n"
        })).await.unwrap();
        let err = execute_builtin(&s, "builtin.code_edit_block", &json!({
            "path": "src/dup.py",
            "old_text": "x = 1",
            "new_text": "y = 2"
        })).await;
        assert!(err.is_err());

        // Missing match: rejected with guidance.
        let err = execute_builtin(&s, "builtin.code_edit_block", &json!({
            "path": "src/main.py",
            "old_text": "not present",
            "new_text": "x"
        })).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn code_list_and_search() {
        let (s, _d) = state().await;
        execute_builtin(&s, "builtin.code_write_file", &json!({ "path": "a/b.py", "content": "value = 42" })).await.unwrap();
        let tree = execute_builtin(&s, "builtin.code_list_dir", &json!({})).await.unwrap();
        assert!(tree["items"].as_array().unwrap().iter().any(|i| i["path"] == "a/b.py"));
        let search = execute_builtin(&s, "builtin.code_search", &json!({ "query": "42" })).await.unwrap();
        assert_eq!(search["matches"].as_array().unwrap().len(), 1);
    }
}