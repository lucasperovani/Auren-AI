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
            session_secret,
        })
    }
}