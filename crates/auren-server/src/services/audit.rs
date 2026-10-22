//! Audit service (spec section 9). Append-only at the application level;
//! never log secrets.

use serde_json::Value;
use sqlx::SqlitePool;

pub struct AuditService {
    db: SqlitePool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorType {
    User,
    Agent,
    System,
}

impl ActorType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActorType::User => "user",
            ActorType::Agent => "agent",
            ActorType::System => "system",
        }
    }
}

/// Redact keys that must never reach the audit log.
const REDACTED_KEYS: [&str; 8] = [
    "password",
    "password_hash",
    "api_key",
    "encrypted_api_key",
    "session_token",
    "token",
    "token_hash",
    "ciphertext",
];

fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if REDACTED_KEYS.iter().any(|r| k.eq_ignore_ascii_case(r)) {
                    out.insert(k.clone(), Value::String("[redacted]".into()));
                } else {
                    out.insert(k.clone(), redact(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

impl AuditService {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// Record an audit event. Failures are logged but never abort the caller:
    /// audit is best-effort for non-critical paths, while critical paths
    /// (approvals) call this inside their own transaction.
    pub async fn record(
        &self,
        task_run_id: Option<&str>,
        event_type: &str,
        actor_type: ActorType,
        actor_id: Option<&str>,
        details: Value,
    ) {
        let details = redact(&details);
        let result = sqlx::query(
            "INSERT INTO audit_events (id, task_run_id, event_type, actor_type, actor_id, details_json, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(task_run_id)
        .bind(event_type)
        .bind(actor_type.as_str())
        .bind(actor_id)
        .bind(details.to_string())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.db)
        .await;
        if let Err(e) = result {
            tracing::error!(event_type, error = %e, "failed to persist audit event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    async fn pool() -> SqlitePool {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        std::mem::forget(dir);
        // The TempDir handle is intentionally leaked above; return a marker
        // so the signature stays clear. The directory lives until process end.
        pool
    }

    #[tokio::test]
    async fn records_events_with_redaction() {
        let pool = pool().await;
        let svc = AuditService::new(pool.clone());
        svc.record(
            None,
            "test.event",
            ActorType::System,
            Some("sys"),
            serde_json::json!({ "api_key": "secret", "task_id": "t1" }),
        )
        .await;
        let row = sqlx::query("SELECT details_json FROM audit_events WHERE event_type = 'test.event'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let details: Value = serde_json::from_str(&row.get::<String, _>("details_json")).unwrap();
        assert_eq!(details["api_key"], "[redacted]");
        assert_eq!(details["task_id"], "t1");
    }
}