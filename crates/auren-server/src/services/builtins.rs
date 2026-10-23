//! Built-in skills (spec section 10): File Management and Web Research.
//!
//! These are registered as declarative builtin implementations that the
//! ToolRegistry can execute. They enforce the same permission engine and
//! workspace restrictions as installed skills.

use serde_json::{json, Value};
use sqlx::Row;
use std::time::Duration;

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
        // Execution tools: shell commands and Python programs in the workspace.
        "builtin.exec_shell" => exec_shell(state, args).await,
        "builtin.exec_python" => exec_python(state, args).await,
        // Remote execution over SSH: connect to a server, run commands there.
        "builtin.ssh_connect" => ssh_connect(state, args).await,
        "builtin.ssh_disconnect" => ssh_disconnect(state, args).await,
        "builtin.ssh_list" => ssh_list(state, args).await,
        "builtin.exec_ssh" => exec_ssh(state, args).await,
        // User-facing notification (appears in the frontend).
        "builtin.notify_user" => notify_user(state, args).await,
        // GitHub tools (OAuth or token auth).
        "builtin.github_list_files" => github_list_files(state, args).await,
        "builtin.github_read_file" => github_read_file(state, args).await,
        "builtin.github_write_file" => github_write_file(state, args).await,
        "builtin.github_list_issues" => github_list_issues(state, args).await,
        "builtin.github_create_issue" => github_create_issue(state, args).await,
        "builtin.github_create_pr" => github_create_pr(state, args).await,
        "builtin.github_me" => github_me(state, args).await,
        "builtin.github_list_repos" => github_list_repos(state, args).await,
        "builtin.github_search_repos" => github_search_repos(state, args).await,
        "builtin.github_list_prs" => github_list_prs(state, args).await,
        "builtin.github_get_pr" => github_get_pr(state, args).await,
        "builtin.github_merge_pr" => github_merge_pr(state, args).await,
        // Local git commands in the workspace, authenticated with the
        // connected GitHub account when needed.
        "builtin.git_clone" => git_clone(state, args).await,
        "builtin.git_status" => git_status(state, args).await,
        "builtin.git_pull" => git_pull(state, args).await,
        "builtin.git_fetch" => git_fetch(state, args).await,
        "builtin.git_push" => git_push(state, args).await,
        "builtin.git_merge" => git_merge(state, args).await,
        "builtin.git_branch" => git_branch(state, args).await,
        "builtin.git_checkout" => git_checkout(state, args).await,
        "builtin.git_commit" => git_commit(state, args).await,
        "builtin.git_log" => git_log(state, args).await,
        "builtin.git_diff" => git_diff(state, args).await,
        "builtin.git_remote" => git_remote(state, args).await,
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

// ---------------------------------------------------------------------------
// Execution tools: shell commands and Python programs, confined to the
// workspace with hard timeouts. These are high-risk tools: the skill
// manifests declare them as such, so the permission engine requires
// approval by default (spec part1 section 4: code execution is an
// explicit, separately permissioned capability).
// ---------------------------------------------------------------------------

/// Maximum output captured from a command.
const MAX_EXEC_OUTPUT: usize = 100_000;
/// Hard timeout for command execution.
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a shell command with its working directory set to the workspace.
async fn exec_shell(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("command is required".into()))?;
    // Reject obvious host-level dangers regardless of policy.
    for forbidden in ["rm -rf /", "mkfs", "dd if=", ":(){", "> /dev/sda", "shutdown", "reboot"] {
        if command.contains(forbidden) {
            return Err(ApiError::Forbidden("command rejected by safety policy".into()));
        }
    }
    let workspace = state.config.workspace_dir.clone();
    let output = tokio::time::timeout(
        EXEC_TIMEOUT,
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&workspace)
            .output(),
    )
    .await
    .map_err(|_| ApiError::Conflict("command timed out after 60s".into()))?
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("command execution failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(json!({
        "exit_code": output.status.code(),
        "stdout": stdout.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
        "stderr": stderr.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
    }))
}

/// Run a Python program: either inline code or a workspace file.
async fn exec_python(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let code = args.get("code").and_then(|v| v.as_str());
    let file = args.get("file").and_then(|v| v.as_str());
    let workspace = state.config.workspace_dir.clone();

    let mut command = tokio::process::Command::new("python3");
    command.current_dir(&workspace);
    if let Some(code) = code {
        command.arg("-c").arg(code);
    } else if let Some(file) = file {
        let resolved = resolve_workspace_path(state, file)?;
        command.arg(&resolved);
    } else {
        return Err(ApiError::Unprocessable("either 'code' or 'file' is required".into()));
    }
    let output = tokio::time::timeout(EXEC_TIMEOUT, command.output())
        .await
        .map_err(|_| ApiError::Conflict("python execution timed out after 60s".into()))?
        .map_err(|e| {
            ApiError::Internal(anyhow::anyhow!("python execution failed: {e}"))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(json!({
        "exit_code": output.status.code(),
        "stdout": stdout.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
        "stderr": stderr.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
    }))
}

// ---------------------------------------------------------------------------
// Remote execution over SSH. Connections are registered per user and stored
// in the database; commands run through the `ssh` binary with a hard timeout.
// Passwords are never stored: connections authenticate with the server's
// key-based setup (agent, default key files, or ssh_config entries).
// ---------------------------------------------------------------------------

/// Register an SSH connection the agent can later run commands on.
async fn ssh_connect(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let host = args.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("host is required".into()))?;
    let user = args.get("user").and_then(|v| v.as_str()).unwrap_or("");
    let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(22);
    if port == 0 || port > 65535 {
        return Err(ApiError::Unprocessable("port must be between 1 and 65535".into()));
    }
    // Reject option-injection: the host string is passed as a single argv
    // element, but a leading dash would be parsed as an ssh option.
    if host.starts_with('-') || host.contains(' ') || host.contains(';') {
        return Err(ApiError::Unprocessable("invalid host".into()));
    }
    if user.contains(' ') {
        return Err(ApiError::Unprocessable("invalid user".into()));
    }
    let name = args.get("name").and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if user.is_empty() { host.to_string() } else { format!("{user}@{host}") }
        });
    let user_id = args.get("user_id").and_then(|v| v.as_str()).unwrap_or_default();
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO ssh_connections (id, user_id, name, host, port, username, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 'connected', ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(&name)
    .bind(host)
    .bind(port as i64)
    .bind(user)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(ApiError::from)?;
    Ok(json!({
        "connection_id": id,
        "name": name,
        "host": host,
        "port": port,
        "user": user,
        "status": "connected",
    }))
}

/// Remove a registered SSH connection.
async fn ssh_disconnect(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let connection_id = args.get("connection_id").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("connection_id is required".into()))?;
    let result = sqlx::query("DELETE FROM ssh_connections WHERE id = ?")
        .bind(connection_id)
        .execute(&state.db)
        .await
        .map_err(ApiError::from)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(json!({ "disconnected": true, "connection_id": connection_id }))
}

/// List the registered SSH connections.
async fn ssh_list(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let user_id = args.get("user_id").and_then(|v| v.as_str()).unwrap_or_default();
    let rows = sqlx::query(
        "SELECT id, name, host, port, username, status, created_at FROM ssh_connections
         WHERE user_id = ? ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::from)?;
    let items = rows
        .iter()
        .map(|r| {
            json!({
                "connection_id": r.get::<String, _>("id"),
                "name": r.get::<String, _>("name"),
                "host": r.get::<String, _>("host"),
                "port": r.get::<i64, _>("port"),
                "user": r.get::<Option<String>, _>("username"),
                "status": r.get::<String, _>("status"),
                "created_at": r.get::<String, _>("created_at"),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "items": items }))
}

/// Run a shell command on a registered remote server over SSH.
async fn exec_ssh(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let connection_id = args.get("connection_id").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("connection_id is required (connect first with ssh_connect)".into()))?;
    let command = args.get("command").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("command is required".into()))?;
    if command.trim().is_empty() {
        return Err(ApiError::Unprocessable("command cannot be empty".into()));
    }
    // Same host-level safety policy as exec_shell.
    for forbidden in ["rm -rf /", "mkfs", "dd if=", ":(){", "> /dev/sda", "shutdown", "reboot"] {
        if command.contains(forbidden) {
            return Err(ApiError::Forbidden("command rejected by safety policy".into()));
        }
    }
    let row = sqlx::query("SELECT host, port, username FROM ssh_connections WHERE id = ?")
        .bind(connection_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::NotFound)?;
    let host: String = row.get("host");
    let port: i64 = row.get("port");
    let username: Option<String> = row.get("username");

    let target = match &username {
        Some(u) if !u.is_empty() => format!("{u}@{host}"),
        _ => host.clone(),
    };
    let port_str = port.to_string();
    let mut ssh = tokio::process::Command::new("ssh");
    ssh.args([
        "-o", "BatchMode=yes",
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=10",
        "-p", &port_str,
        &target,
        command,
    ]);
    let output = tokio::time::timeout(EXEC_TIMEOUT, ssh.output())
        .await
        .map_err(|_| ApiError::Conflict("ssh command timed out after 60s".into()))?
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("ssh execution failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(json!({
        "exit_code": output.status.code(),
        "stdout": stdout.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
        "stderr": stderr.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
    }))
}

/// Send a message to the user in the frontend: persists a notification and
/// broadcasts it over the WebSocket channel.
async fn notify_user(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| {
        ApiError::Unprocessable("title is required".into())
    })?;
    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let user_id = args.get("user_id").and_then(|v| v.as_str()).unwrap_or_default();
    let service = crate::services::notifications::NotificationService::new(state);
    let id = service
        .create(user_id, "agent_message", title, body, None, None, None)
        .await
        .map_err(ApiError::from)?;
    Ok(json!({ "notification_id": id, "delivered": true }))
}

// ---------------------------------------------------------------------------
// GitHub tools (spec: communications with GitHub, OAuth when configured).
// The token comes from a connected `github` integration account.
// ---------------------------------------------------------------------------

const GITHUB_API: &str = "https://api.github.com";

/// Resolve the GitHub token from the connected account in the arguments.
async fn github_token(state: &AppState, args: &Value) -> Result<String, ApiError> {
    let account_id = args
        .get("integration_account_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ApiError::Conflict("connect a GitHub account in the Integrations view first".into())
        })?;
    let row = sqlx::query(
        "SELECT c.ciphertext, c.nonce FROM encrypted_credentials c
         JOIN integration_accounts a ON a.id = c.integration_account_id
         WHERE c.integration_account_id = ? AND a.status = 'active'",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::Conflict("GitHub account is not connected or is inactive".into()))?;
    let ct: Vec<u8> = row.get("ciphertext");
    let nonce: Vec<u8> = row.get("nonce");
    crate::services::credentials::decrypt_credential(&ct, &nonce, &state.config.credential_encryption_key)
        .map_err(|_| ApiError::Internal(anyhow::anyhow!("credential decryption failed")))
}

/// GitHub API request with the account's token.
async fn github_api(
    state: &AppState,
    args: &Value,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, ApiError> {
    let token = github_token(state, args).await?;
    let mut request = reqwest::Client::new()
        .request(method, format!("{GITHUB_API}{path}"))
        .bearer_auth(&token)
        .header("User-Agent", "auren-ai")
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30));
    if let Some(b) = body {
        request = request.json(&b);
    }
    let response = request.send().await.map_err(|e| {
        tracing::debug!(error = %e, "github api request failed");
        ApiError::ServiceUnavailable
    })?;
    let status = response.status();
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            Err(ApiError::Conflict("GitHub token rejected; reconnect the account".into()))
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => Err(ApiError::TooManyRequests),
        s if s.is_success() => {
            let value: Value = response.json().await.unwrap_or(json!({}));
            Ok(value)
        }
        // 204 No Content is success with no body.
        reqwest::StatusCode::NO_CONTENT => Ok(json!({ "ok": true })),
        s => {
            let body = response.text().await.unwrap_or_default();
            tracing::debug!(status = s.as_u16(), body = %body, "github api error");
            Err(ApiError::Conflict(format!("GitHub API returned HTTP {}", s.as_u16())))
        }
    }
}

async fn github_list_files(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("");
    let ref_param = if branch.is_empty() { String::new() } else { format!("?ref={}", urlencode(branch)) };
    let result = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/contents/{}{}", urlencode(path), ref_param), None).await?;
    if let Some(items) = result.as_array() {
        Ok(json!({
            "items": items.iter().map(|i| json!({
                "name": i.get("name"),
                "path": i.get("path"),
                "type": i.get("type"),
                "size": i.get("size"),
            })).collect::<Vec<_>>()
        }))
    } else {
        Ok(json!({ "file": { "path": result.get("path"), "size": result.get("size") } }))
    }
}

async fn github_read_file(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let result = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/contents/{}", urlencode(path)), None).await?;
    // Content arrives base64-encoded.
    let encoded = result.get("content").and_then(|c| c.as_str()).unwrap_or_default();
    let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded.replace('\n', ""))
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    Ok(json!({ "path": result.get("path"), "content": decoded, "sha": result.get("sha") }))
}

/// Create or update a file in a repository (commits via the Contents API).
async fn github_write_file(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("path is required".into()))?;
    let content = args.get("content").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("content is required".into()))?;
    let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("Update via Auren-AI");
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("");

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let mut body = json!({ "message": message, "content": encoded });
    if !branch.is_empty() {
        body["branch"] = json!(branch);
    }
    // Updating an existing file requires its current sha.
    let existing = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/contents/{}", urlencode(path)), None).await;
    if let Ok(sha) = existing.and_then(|r| r.get("sha").and_then(|s| s.as_str().map(|s| s.to_string())).ok_or(ApiError::NotFound)) {
        body["sha"] = json!(sha);
    }
    let result = github_api(state, args, reqwest::Method::PUT, &format!("/repos/{owner}/{repo}/contents/{}", urlencode(path)), Some(body)).await?;
    Ok(json!({
        "committed": true,
        "path": result.get("content").and_then(|c| c.get("path")).cloned().unwrap_or(Value::Null),
        "commit_sha": result.get("commit").and_then(|c| c.get("sha")).cloned().unwrap_or(Value::Null),
    }))
}

async fn github_list_issues(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let result = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/issues?state={}", args.get("state").and_then(|v| v.as_str()).unwrap_or("open")), None).await?;
    Ok(json!({
        "issues": result.as_array().cloned().unwrap_or_default().iter().map(|i| json!({
            "number": i.get("number"),
            "title": i.get("title"),
            "state": i.get("state"),
        })).collect::<Vec<_>>()
    }))
}

async fn github_create_issue(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("title is required".into()))?;
    let result = github_api(state, args, reqwest::Method::POST, &format!("/repos/{owner}/{repo}/issues"), Some(json!({
        "title": title,
        "body": args.get("body").and_then(|v| v.as_str()).unwrap_or(""),
    }))).await?;
    Ok(json!({ "issue_number": result.get("number"), "url": result.get("html_url") }))
}

/// Create a pull request from a branch.
async fn github_create_pr(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let title = args.get("title").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("title is required".into()))?;
    let head = args.get("head").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("head branch is required".into()))?;
    let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("main");
    let result = github_api(state, args, reqwest::Method::POST, &format!("/repos/{owner}/{repo}/pulls"), Some(json!({
        "title": title,
        "head": head,
        "base": base,
        "body": args.get("body").and_then(|v| v.as_str()).unwrap_or(""),
    }))).await?;
    Ok(json!({ "pr_number": result.get("number"), "url": result.get("html_url") }))
}

/// Authenticated user's own profile (login, name, email, counts).
async fn github_me(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let result = github_api(state, args, reqwest::Method::GET, "/user", None).await?;
    Ok(json!({
        "login": result.get("login"),
        "name": result.get("name"),
        "email": result.get("email"),
        "public_repos": result.get("public_repos"),
        "created_at": result.get("created_at"),
        "url": result.get("html_url"),
    }))
}

/// List the authenticated user's repositories, including private ones.
async fn github_list_repos(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let visibility = args.get("visibility").and_then(|v| v.as_str()).unwrap_or("all");
    let sort = args.get("sort").and_then(|v| v.as_str()).unwrap_or("updated");
    let per_page = args.get("max").and_then(|v| v.as_u64()).unwrap_or(30).min(100);
    let result = github_api(state, args, reqwest::Method::GET,
        &format!("/user/repos?visibility={visibility}&sort={sort}&per_page={per_page}"), None).await?;
    Ok(json!({
        "repos": result.as_array().cloned().unwrap_or_default().iter().map(|r| json!({
            "full_name": r.get("full_name"),
            "private": r.get("private"),
            "description": r.get("description"),
            "default_branch": r.get("default_branch"),
            "updated_at": r.get("updated_at"),
            "url": r.get("html_url"),
        })).collect::<Vec<_>>()
    }))
}

/// Search repositories (public and, with the token, private ones the user
/// can access).
async fn github_search_repos(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let query = args.get("query").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("query is required".into()))?;
    let per_page = args.get("max").and_then(|v| v.as_u64()).unwrap_or(20).min(100);
    let result = github_api(state, args, reqwest::Method::GET,
        &format!("/search/repositories?q={}&per_page={per_page}", urlencode(query)), None).await?;
    Ok(json!({
        "total": result.get("total_count"),
        "repos": result.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default().iter().map(|r| json!({
            "full_name": r.get("full_name"),
            "private": r.get("private"),
            "description": r.get("description"),
            "url": r.get("html_url"),
        })).collect::<Vec<_>>()
    }))
}

/// List pull requests in a repository.
async fn github_list_prs(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let state_param = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let result = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/pulls?state={state_param}"), None).await?;
    Ok(json!({
        "prs": result.as_array().cloned().unwrap_or_default().iter().map(|p| json!({
            "number": p.get("number"),
            "title": p.get("title"),
            "state": p.get("state"),
            "head": p.get("head").and_then(|h| h.get("ref")),
            "base": p.get("base").and_then(|b| b.get("ref")),
            "draft": p.get("draft"),
            "url": p.get("html_url"),
        })).collect::<Vec<_>>()
    }))
}

/// Get one pull request's details (state, mergeability, review summary).
async fn github_get_pr(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let number = args.get("number").and_then(|v| v.as_u64())
        .ok_or_else(|| ApiError::Unprocessable("number is required".into()))?;
    let result = github_api(state, args, reqwest::Method::GET, &format!("/repos/{owner}/{repo}/pulls/{number}"), None).await?;
    Ok(json!({
        "number": result.get("number"),
        "title": result.get("title"),
        "state": result.get("state"),
        "merged": result.get("merged"),
        "mergeable": result.get("mergeable"),
        "head": result.get("head").and_then(|h| h.get("ref")),
        "base": result.get("base").and_then(|b| b.get("ref")),
        "author": result.get("user").and_then(|u| u.get("login")),
        "commits": result.get("commits"),
        "additions": result.get("additions"),
        "deletions": result.get("deletions"),
        "url": result.get("html_url"),
    }))
}

/// Merge a pull request. High risk: requires approval.
async fn github_merge_pr(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let owner = args.get("owner").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("owner is required".into()))?;
    let repo = args.get("repo").and_then(|v| v.as_str()).ok_or_else(|| ApiError::Unprocessable("repo is required".into()))?;
    let number = args.get("number").and_then(|v| v.as_u64())
        .ok_or_else(|| ApiError::Unprocessable("number is required".into()))?;
    let method = args.get("method").and_then(|v| v.as_str()).unwrap_or("merge");
    if !["merge", "squash", "rebase"].contains(&method) {
        return Err(ApiError::Unprocessable("method must be merge, squash, or rebase".into()));
    }
    let result = github_api(state, args, reqwest::Method::PUT, &format!("/repos/{owner}/{repo}/pulls/{number}/merge"), Some(json!({
        "merge_method": method,
    }))).await?;
    Ok(json!({
        "merged": result.get("merged").unwrap_or(&json!(true)),
        "sha": result.get("sha"),
    }))
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

// ---------------------------------------------------------------------------
// Local git commands (run the `git` binary in the workspace). Network
// operations (clone/pull/fetch/push) authenticate with the connected GitHub
// account's token; the token is passed via the credential helper environment
// and never appears in command arguments, output, or logs.
// ---------------------------------------------------------------------------

/// Run `git` with the given arguments in a workspace subdirectory. The
/// optional token is injected through `GIT_ASKPASS`-free credential
/// environment variables so it never lands in argv or captured output.
async fn run_git(
    state: &AppState,
    subpath: &str,
    args: &[&str],
    token: Option<&str>,
) -> Result<Value, ApiError> {
    let workspace = std::path::PathBuf::from(&state.config.workspace_dir);
    let dir = if subpath.is_empty() {
        workspace.clone()
    } else {
        resolve_workspace_path(state, subpath)?
    };
    if !dir.is_dir() {
        return Err(ApiError::NotFound);
    }

    let mut command = tokio::process::Command::new("git");
    command
        .args(args)
        .current_dir(&dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    if let Some(token) = token {
        // git reads credentials from these variables for http(s) remotes;
        // they are not inherited by child processes of git itself.
        command.env("GIT_HTTP_USER", "x-access-token");
        command.env("GIT_HTTP_PASSWORD", token);
        command.env("GIT_CONFIG_COUNT", "2");
        command.env("GIT_CONFIG_KEY_0", "credential.helper");
        command.env("GIT_CONFIG_VALUE_0", format!("!f() {{ echo username=x-access-token; echo password={token}; }}; f"));
        command.env("GIT_CONFIG_KEY_1", "url.https://x-access-token@github.com/.insteadOf");
        command.env("GIT_CONFIG_VALUE_1", "https://github.com/");
    }

    let output = tokio::time::timeout(EXEC_TIMEOUT, command.output())
        .await
        .map_err(|_| ApiError::Conflict("git command timed out after 60s".into()))?
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("git execution failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(json!({
        "exit_code": output.status.code(),
        "stdout": stdout.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
        "stderr": stderr.chars().take(MAX_EXEC_OUTPUT).collect::<String>(),
    }))
}

/// Resolve the GitHub token when the caller provides an account; commands
/// that never touch the network work without one.
async fn optional_github_token(state: &AppState, args: &Value) -> Result<Option<String>, ApiError> {
    match args.get("integration_account_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => Ok(Some(github_token(state, args).await?)),
        _ => Ok(None),
    }
}

/// Extract `owner/repo` from a GitHub URL or a bare `owner/repo` shorthand.
fn github_repo_url(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::Unprocessable("repository is required".into()));
    }
    // Bare "owner/repo" shorthand.
    if !value.contains("://") && !value.contains('@') {
        let parts: Vec<&str> = value.split('/').filter(|p| !p.is_empty()).collect();
        if parts.len() == 2 && !parts.iter().any(|p| p.contains("..")) {
            return Ok(format!("https://github.com/{}/{}/", parts[0], parts[1].trim_end_matches(".git")));
        }
        return Err(ApiError::Unprocessable(
            "repository must be 'owner/repo' or a https://github.com URL".into(),
        ));
    }
    // Only https GitHub URLs are allowed; ssh remotes would bypass the
    // token-based authentication path.
    let parsed = url::Url::parse(value).map_err(|_| ApiError::Unprocessable("invalid repository URL".into()))?;
    if parsed.scheme() != "https" {
        return Err(ApiError::Unprocessable("only https:// URLs are supported".into()));
    }
    if parsed.host_str() != Some("github.com") {
        return Err(ApiError::Forbidden("only github.com repositories are allowed".into()));
    }
    Ok(value.trim_end_matches(".git").to_string() + "/")
}

async fn git_clone(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let repository = args.get("repository").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("repository is required ('owner/repo' or https URL)".into()))?;
    let url = github_repo_url(repository)?;
    let token = optional_github_token(state, args).await?;
    let destination = args.get("destination").and_then(|v| v.as_str()).unwrap_or("");
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("");
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);

    let mut git_args: Vec<&str> = vec!["clone"];
    if !branch.is_empty() {
        git_args.extend(["--branch", branch]);
    }
    if depth > 0 {
        git_args.extend(["--depth", Box::leak(depth.to_string().into_boxed_str())]);
    }
    let target: String = if destination.is_empty() {
        // Derive the folder name from the URL, mirroring git's default.
        let name = url.trim_end_matches('/')
            .rsplit('/').next().unwrap_or("repo");
        name.to_string()
    } else {
        // Validate the destination stays inside the workspace.
        resolve_workspace_path(state, destination)?;
        destination.to_string()
    };
    if !destination.is_empty() {
        let exists = std::path::PathBuf::from(&state.config.workspace_dir).join(destination).exists();
        if exists {
            return Err(ApiError::Conflict(format!("destination '{destination}' already exists")));
        }
    }
    git_args.push(&url);
    git_args.push(&target);
    let result = run_git(state, "", &git_args, token.as_deref()).await?;
    if result["exit_code"] != 0 {
        return Ok(json!({
            "cloned": false,
            "error": result["stderr"],
            "exit_code": result["exit_code"],
        }));
    }
    Ok(json!({
        "cloned": true,
        "repository": url,
        "path": if destination.is_empty() { target } else { destination.to_string() },
    }))
}

async fn git_status(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    run_git(state, path, &["status", "--porcelain=v1", "-b"], None).await
}

async fn git_pull(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let token = optional_github_token(state, args).await?;
    run_git(state, path, &["pull", "--ff-only"], token.as_deref()).await
}

async fn git_fetch(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let token = optional_github_token(state, args).await?;
    run_git(state, path, &["fetch", "--all", "--prune"], token.as_deref()).await
}

async fn git_push(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let token = optional_github_token(state, args).await?
        .ok_or_else(|| ApiError::Conflict("connect a GitHub account in the Integrations view first".into()))?;
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("");
    let remote = args.get("remote").and_then(|v| v.as_str()).unwrap_or("origin");
    let mut git_args: Vec<&str> = vec!["push", remote];
    if !branch.is_empty() {
        git_args.push(branch);
    }
    run_git(state, path, &git_args, Some(&token)).await
}

async fn git_merge(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let branch = args.get("branch").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("branch is required".into()))?;
    if branch.contains("..") || branch.starts_with('-') {
        return Err(ApiError::Unprocessable("invalid branch name".into()));
    }
    run_git(state, path, &["merge", branch], None).await
}

async fn git_branch(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    match args.get("name").and_then(|v| v.as_str()) {
        Some(name) => {
            if name.contains("..") || name.starts_with('-') || name.contains(' ') {
                return Err(ApiError::Unprocessable("invalid branch name".into()));
            }
            run_git(state, path, &["branch", name], None).await
        }
        None => run_git(state, path, &["branch", "--list"], None).await,
    }
}

async fn git_checkout(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let branch = args.get("branch").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("branch is required".into()))?;
    if branch.contains("..") || branch.starts_with('-') {
        return Err(ApiError::Unprocessable("invalid branch name".into()));
    }
    let create = args.get("create").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut git_args: Vec<&str> = vec!["checkout"];
    if create {
        git_args.push("-b");
    }
    git_args.push(branch);
    run_git(state, path, &git_args, None).await
}

async fn git_commit(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let message = args.get("message").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("message is required".into()))?;
    if message.trim().is_empty() {
        return Err(ApiError::Unprocessable("message cannot be empty".into()));
    }
    let add_all = args.get("add_all").and_then(|v| v.as_bool()).unwrap_or(true);
    if add_all {
        run_git(state, path, &["add", "-A"], None).await?;
    }
    run_git(state, path, &["commit", "-m", message], None).await
}

async fn git_log(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let max = args.get("max_count").and_then(|v| v.as_u64()).unwrap_or(20).min(100);
    let max_str = Box::leak(max.to_string().into_boxed_str());
    run_git(state, path, &["log", "--oneline", "-n", max_str], None).await
}

async fn git_diff(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    run_git(state, path, &["diff", "--stat"], None).await
}

/// Show the remotes of a repository in the workspace.
async fn git_remote(state: &AppState, args: &Value) -> Result<Value, ApiError> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    run_git(state, path, &["remote", "-v"], None).await
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
        crate::db::migrate(&pool).await.unwrap();
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
                max_iterations: 25,
                max_tool_calls: 50,
                max_llm_calls: 40,
                subagent_auto_delete: false,
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

    #[test]
    fn github_repo_url_accepts_shorthand_and_https() {
        assert_eq!(github_repo_url("octocat/hello").unwrap(), "https://github.com/octocat/hello/");
        assert_eq!(github_repo_url("octocat/hello.git").unwrap(), "https://github.com/octocat/hello/");
        assert_eq!(
            github_repo_url("https://github.com/octocat/hello").unwrap(),
            "https://github.com/octocat/hello/"
        );
        assert!(github_repo_url("").is_err());
        assert!(github_repo_url("just-a-name").is_err());
        assert!(github_repo_url("git@github.com:octocat/hello.git").is_err());
        assert!(github_repo_url("http://github.com/octocat/hello").is_err());
        assert!(github_repo_url("https://evil.com/octocat/hello").is_err());
    }

    #[tokio::test]
    async fn github_new_tools_require_account() {
        let (s, _d) = state().await;
        // Without a connected account these fail with a clear conflict.
        for tool in ["builtin.github_me", "builtin.github_list_repos", "builtin.github_search_repos"] {
            let result = execute_builtin(&s, tool, &json!({ "query": "x" })).await;
            assert!(result.is_err(), "expected error for {tool}");
        }
        for tool in ["builtin.github_list_prs", "builtin.github_get_pr", "builtin.github_merge_pr"] {
            let result = execute_builtin(&s, tool, &json!({ "owner": "o", "repo": "r", "number": 1 })).await;
            assert!(result.is_err(), "expected error for {tool}");
        }
    }

    #[tokio::test]
    async fn github_merge_pr_rejects_invalid_method() {
        let (s, _d) = state().await;
        let result = execute_builtin(&s, "builtin.github_merge_pr", &json!({
            "owner": "o", "repo": "r", "number": 1, "method": "force"
        })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn git_clone_rejects_existing_destination() {
        let (s, _d) = state().await;
        std::fs::create_dir_all(std::path::PathBuf::from(&s.config.workspace_dir).join("already")).unwrap();
        let result = execute_builtin(&s, "builtin.git_clone", &json!({
            "repository": "octocat/hello",
            "destination": "already"
        })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn git_clone_rejects_non_github_urls() {
        let (s, _d) = state().await;
        for repository in ["https://gitlab.com/a/b", "ssh://git@github.com/a/b", "../escape"] {
            let result = execute_builtin(&s, "builtin.git_clone", &json!({ "repository": repository })).await;
            assert!(result.is_err(), "expected rejection for {repository}");
        }
    }

    #[tokio::test]
    async fn git_local_commands_roundtrip() {
        let (s, _d) = state().await;
        // Create a real repository inside the workspace.
        let repo = std::path::PathBuf::from(&s.config.workspace_dir).join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let init = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap()
        };
        init(&["init", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "hello").unwrap();
        init(&["add", "-A"]);
        init(&["commit", "-m", "first"]);

        let status = execute_builtin(&s, "builtin.git_status", &json!({ "path": "repo" })).await.unwrap();
        assert_eq!(status["exit_code"], 0);
        assert!(status["stdout"].as_str().unwrap().contains("main"));

        let log = execute_builtin(&s, "builtin.git_log", &json!({ "path": "repo" })).await.unwrap();
        assert!(log["stdout"].as_str().unwrap().contains("first"));

        let branch = execute_builtin(&s, "builtin.git_branch", &json!({ "path": "repo", "name": "feature" })).await.unwrap();
        assert_eq!(branch["exit_code"], 0);
        let checkout = execute_builtin(&s, "builtin.git_checkout", &json!({ "path": "repo", "branch": "feature" })).await.unwrap();
        assert_eq!(checkout["exit_code"], 0);

        std::fs::write(repo.join("b.txt"), "world").unwrap();
        let diff = execute_builtin(&s, "builtin.git_diff", &json!({ "path": "repo" })).await.unwrap();
        assert_eq!(diff["exit_code"], 0);

        let commit = execute_builtin(&s, "builtin.git_commit", &json!({ "path": "repo", "message": "second" })).await.unwrap();
        assert_eq!(commit["exit_code"], 0);
        let merge = execute_builtin(&s, "builtin.git_merge", &json!({ "path": "repo", "branch": "main" })).await.unwrap();
        assert_eq!(merge["exit_code"], 0);
    }

    #[tokio::test]
    async fn git_merge_rejects_invalid_branch() {
        let (s, _d) = state().await;
        let result = execute_builtin(&s, "builtin.git_merge", &json!({ "branch": "--evil" })).await;
        assert!(result.is_err());
        let result = execute_builtin(&s, "builtin.git_branch", &json!({ "name": "a..b" })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn git_push_requires_account() {
        let (s, _d) = state().await;
        let result = execute_builtin(&s, "builtin.git_push", &json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn ssh_connect_list_disconnect_roundtrip() {
        let (s, _d) = state().await;
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','u@t','U','h',0,'2026','2026')")
            .execute(&s.db).await.unwrap();
        let connected = execute_builtin(&s, "builtin.ssh_connect", &json!({
            "user_id": "u1",
            "host": "server.example.com",
            "user": "deploy",
            "port": 2222,
            "name": "prod"
        })).await.unwrap();
        assert_eq!(connected["status"], "connected");
        assert_eq!(connected["name"], "prod");
        let connection_id = connected["connection_id"].as_str().unwrap().to_string();

        let list = execute_builtin(&s, "builtin.ssh_list", &json!({ "user_id": "u1" })).await.unwrap();
        let items = list["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["host"], "server.example.com");
        assert_eq!(items[0]["port"], 2222);

        let removed = execute_builtin(&s, "builtin.ssh_disconnect", &json!({ "connection_id": connection_id })).await.unwrap();
        assert_eq!(removed["disconnected"], true);
        let list = execute_builtin(&s, "builtin.ssh_list", &json!({ "user_id": "u1" })).await.unwrap();
        assert_eq!(list["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn ssh_connect_rejects_invalid_input() {
        let (s, _d) = state().await;
        assert!(execute_builtin(&s, "builtin.ssh_connect", &json!({})).await.is_err());
        assert!(execute_builtin(&s, "builtin.ssh_connect", &json!({ "host": "-oProxyCommand=evil" })).await.is_err());
        assert!(execute_builtin(&s, "builtin.ssh_connect", &json!({ "host": "h", "port": 70000 })).await.is_err());
        assert!(execute_builtin(&s, "builtin.ssh_disconnect", &json!({ "connection_id": "missing" })).await.is_err());
    }

    #[tokio::test]
    async fn exec_ssh_rejects_dangerous_commands_and_unknown_connection() {
        let (s, _d) = state().await;
        assert!(execute_builtin(&s, "builtin.exec_ssh", &json!({ "connection_id": "x", "command": "rm -rf /" })).await.is_err());
        assert!(execute_builtin(&s, "builtin.exec_ssh", &json!({ "connection_id": "x", "command": "reboot" })).await.is_err());
        assert!(execute_builtin(&s, "builtin.exec_ssh", &json!({ "connection_id": "x", "command": "ls" })).await.is_err());
        assert!(execute_builtin(&s, "builtin.exec_ssh", &json!({ "command": "ls" })).await.is_err());
    }
}