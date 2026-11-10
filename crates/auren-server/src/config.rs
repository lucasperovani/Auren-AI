use serde::Deserialize;

/// Application configuration, loaded from environment variables.
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    /// Path to the SQLite database file.
    pub database_url: String,
    /// Address to bind the HTTP server to.
    pub bind_addr: String,
    /// Directory where validated skill packages are installed.
    pub skills_dir: String,
    /// Maximum accepted skill upload size in bytes.
    pub max_upload_bytes: usize,
    /// Secret used to hash session tokens (HMAC-style pepper). Must be set in production.
    pub session_secret: String,
    /// Agent workspace directory (spec section 4). Defaults to
    /// `<data>/workspace`; a host directory may be mounted and configured.
    pub workspace_dir: String,
    /// Maximum size of a single workspace file operation in bytes.
    pub max_workspace_file_bytes: u64,
    /// Key used to encrypt credentials at rest (spec section 14). When
    /// empty, a key is derived from the session secret (development only).
    pub credential_encryption_key: String,
    /// Maximum iterations of a task-run/chat LLM loop.
    pub max_iterations: u32,
    /// Maximum tool calls within one execution.
    pub max_tool_calls: u32,
    /// Maximum LLM calls within one execution.
    pub max_llm_calls: u32,
    /// Delete subagents after a successful delegation (default: keep).
    pub subagent_auto_delete: bool,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let session_secret = std::env::var("AUREN_SESSION_SECRET").unwrap_or_default();
        Ok(Self {
            database_url: std::env::var("AUREN_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://data/auren.db?mode=rwc".to_string()),
            bind_addr: std::env::var("AUREN_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
            skills_dir: std::env::var("AUREN_SKILLS_DIR").unwrap_or_else(|_| "data/skills".to_string()),
            max_upload_bytes: std::env::var("AUREN_MAX_UPLOAD_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(50 * 1024 * 1024),
            workspace_dir: std::env::var("AI_WORKSPACE_CONTAINER_PATH")
                .or_else(|_| std::env::var("AUREN_WORKSPACE_DIR"))
                .unwrap_or_else(|_| "data/workspace".to_string()),
            max_workspace_file_bytes: std::env::var("AUREN_MAX_WORKSPACE_FILE_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 1024 * 1024),
            credential_encryption_key: std::env::var("AUREN_CREDENTIAL_ENCRYPTION_KEY")
                .unwrap_or_else(|_| session_secret.clone()),
            max_iterations: std::env::var("AUREN_MAX_ITERATIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(25),
            max_tool_calls: std::env::var("AUREN_MAX_TOOL_CALLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(50),
            max_llm_calls: std::env::var("AUREN_MAX_LLM_CALLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(40),
            subagent_auto_delete: std::env::var("AUREN_SUBAGENT_AUTO_DELETE")
                .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
                .unwrap_or(false),
            session_secret,
        })
    }
}

/// Extract the filesystem path of a SQLite database URL.
///
/// Accepts the forms SQLx supports — `sqlite:data/auren.db`, `sqlite://data/auren.db`,
/// `sqlite:///data/auren.db` (absolute), and `sqlite::memory:` — and returns
/// `None` for anything that has no file behind it (in-memory or a non-SQLite
/// URL). Query parameters such as `?mode=rwc` are stripped, so callers never
/// treat `sqlite://x` as a directory named `sqlite:`.
pub fn sqlite_file_path(database_url: &str) -> Option<std::path::PathBuf> {
    let rest = database_url.strip_prefix("sqlite:")?;
    // In-memory databases have no path (and must not create one).
    if rest.starts_with(":memory:") {
        return None;
    }
    // Drop the query string (`?mode=rwc&cache=shared`).
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() {
        return None;
    }
    // `sqlite:///abs/path` -> `/abs/path`; `sqlite://rel/path` -> `rel/path`.
    let path = match rest.strip_prefix("//") {
        Some(after) => after,
        None => rest,
    };
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::sqlite_file_path;

    #[test]
    fn extracts_filesystem_path_from_sqlite_urls() {
        // The bug: treating the URL as a path created a `sqlite:` directory.
        assert_eq!(
            sqlite_file_path("sqlite://data/auren.db?mode=rwc").as_deref(),
            Some(std::path::Path::new("data/auren.db"))
        );
        assert_eq!(
            sqlite_file_path("sqlite:data/auren.db").as_deref(),
            Some(std::path::Path::new("data/auren.db"))
        );
        // Triple slash means an absolute path.
        assert_eq!(
            sqlite_file_path("sqlite:///data/auren.db?mode=rwc").as_deref(),
            Some(std::path::Path::new("/data/auren.db"))
        );
        // In-memory and non-SQLite URLs have no file.
        assert!(sqlite_file_path("sqlite::memory:").is_none());
        assert!(sqlite_file_path("sqlite://").is_none());
        assert!(sqlite_file_path("postgres://localhost/db").is_none());
    }
}