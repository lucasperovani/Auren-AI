//! Approval service (spec sections 2.2 and 3.11).
//!
//! Approvals are bound to an immutable canonical operation payload and its
//! SHA-256 hash. Any material change to the operation invalidates the
//! approval; the server revalidates before execution.

use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{error::ApiError, events::EventEnvelope, state::AppState};

/// How long a pending approval stays valid.
const APPROVAL_TTL_SECS: i64 = 15 * 60;

pub struct ApprovalService {
    db: SqlitePool,
    events: crate::events::SharedEventBus,
}

/// Canonical hash of an operation payload.
pub fn operation_hash(operation: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(operation).unwrap_or_default();
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

impl ApprovalService {
    pub fn new(state: &AppState) -> Self {
        Self { db: state.db.clone(), events: state.events.clone() }
    }

    /// Create a pending approval request for an operation.
    pub async fn create(
        &self,
        task_run_id: Option<&str>,
        skill_tool_id: Option<&str>,
        operation: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now();
        sqlx::query(
            "INSERT INTO approval_requests (id, task_run_id, skill_tool_id, operation_hash, operation_json, status, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&id)
        .bind(task_run_id)
        .bind(skill_tool_id)
        .bind(operation_hash(operation))
        .bind(operation.to_string())
        .bind((now + chrono::Duration::seconds(APPROVAL_TTL_SECS)).to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.db)
        .await?;

        self.events.publish(EventEnvelope::new(
            "task_run.approval_required",
            serde_json::json!({ "approval_id": id, "task_run_id": task_run_id }),
        ));
        Ok(id)
    }

    /// Approve a pending request. The stored operation hash is verified
    /// against the operation the caller believes it is approving; a mismatch
    /// invalidates the approval (spec: "a changed operation must require a
    /// new approval").
    pub async fn decide(
        &self,
        approval_id: &str,
        user_id: &str,
        decision: &str,
        comment: Option<&str>,
        expected_operation: Option<&serde_json::Value>,
    ) -> Result<(), ApiError> {
        let row = sqlx::query(
            "SELECT operation_hash, operation_json, status, expires_at FROM approval_requests WHERE id = ?",
        )
        .bind(approval_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        let status: String = row.get("status");
        if status != "pending" {
            return Err(ApiError::Conflict("approval is not pending".into()));
        }
        let expires_at: String = row.get("expires_at");
        let expires = chrono::DateTime::parse_from_rfc3339(&expires_at)
            .map_err(|_| ApiError::Internal(anyhow::anyhow!("invalid approval expiry")))?;
        if expires < chrono::Utc::now() {
            sqlx::query("UPDATE approval_requests SET status = 'expired' WHERE id = ?")
                .bind(approval_id)
                .execute(&self.db)
                .await?;
            return Err(ApiError::Conflict("approval expired".into()));
        }

        if let Some(expected) = expected_operation {
            let stored_hash: String = row.get("operation_hash");
            if stored_hash != operation_hash(expected) {
                // The operation changed since the request was created.
                sqlx::query("UPDATE approval_requests SET status = 'cancelled' WHERE id = ?")
                    .bind(approval_id)
                    .execute(&self.db)
                    .await?;
                return Err(ApiError::Conflict(
                    "operation changed since the approval was requested; a new approval is required".into(),
                ));
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.db.begin().await.map_err(ApiError::from)?;
        sqlx::query("UPDATE approval_requests SET status = ? WHERE id = ? AND status = 'pending'")
            .bind(decision)
            .bind(approval_id)
            .execute(&mut *tx)
            .await?;
        // UNIQUE(approval_request_id) prevents a second decision.
        sqlx::query(
            "INSERT INTO approval_decisions (id, approval_request_id, user_id, decision, comment, decided_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(approval_id)
        .bind(user_id)
        .bind(decision)
        .bind(comment)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await.map_err(ApiError::from)?;

        self.events.publish(EventEnvelope::new(
            "approval.updated",
            serde_json::json!({ "approval_id": approval_id, "status": decision }),
        ));
        Ok(())
    }

    /// Revalidate a decision before execution: the approval must be
    /// `approved` and the operation hash must still match.
    pub async fn revalidate(
        &self,
        approval_id: &str,
        operation: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let row = sqlx::query("SELECT status, operation_hash FROM approval_requests WHERE id = ?")
            .bind(approval_id)
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let status: String = row.get("status");
        if status != "approved" {
            return Err(ApiError::Forbidden("approval is not in approved state".into()));
        }
        let stored: String = row.get("operation_hash");
        if stored != operation_hash(operation) {
            return Err(ApiError::Forbidden("operation no longer matches the approved payload".into()));
        }
        Ok(())
    }

    /// Cancel a pending approval (by the owner or the system).
    pub async fn cancel(&self, approval_id: &str) -> Result<(), ApiError> {
        let result = sqlx::query("UPDATE approval_requests SET status = 'cancelled' WHERE id = ? AND status = 'pending'")
            .bind(approval_id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::Conflict("approval is not pending".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                max_workspace_file_bytes: 1024 * 1024,
                credential_encryption_key: "k".into(),
            },
            events: std::sync::Arc::new(crate::events::EventBus::new()),
            ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
        }
    }

    #[tokio::test]
    async fn approve_and_revalidate() {
        let s = state().await;
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&s.db).await.unwrap();
        let svc = ApprovalService::new(&s);
        let op = serde_json::json!({ "tool": "gmail.search", "args": { "q": "invoice" } });

        let id = svc.create(None, None, &op).await.unwrap();
        svc.decide(&id, "u1", "approved", None, Some(&op)).await.unwrap();
        // Revalidation passes for the same operation.
        svc.revalidate(&id, &op).await.unwrap();
        // But fails for a changed operation.
        let changed = serde_json::json!({ "tool": "gmail.search", "args": { "q": "other" } });
        assert!(svc.revalidate(&id, &changed).await.is_err());
    }

    #[tokio::test]
    async fn changed_operation_invalidates_approval() {
        let s = state().await;
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&s.db).await.unwrap();
        let svc = ApprovalService::new(&s);
        let op = serde_json::json!({ "tool": "t", "args": {} });
        let id = svc.create(None, None, &op).await.unwrap();

        // Approving against a different operation cancels the request.
        let other = serde_json::json!({ "tool": "t", "args": { "x": 1 } });
        assert!(svc.decide(&id, "u1", "approved", None, Some(&other)).await.is_err());
        let status: String = sqlx::query("SELECT status FROM approval_requests WHERE id = ?")
            .bind(&id).fetch_one(&s.db).await.unwrap().get("status");
        assert_eq!(status, "cancelled");
    }

    #[tokio::test]
    async fn single_decision_only() {
        let s = state().await;
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&s.db).await.unwrap();
        let svc = ApprovalService::new(&s);
        let op = serde_json::json!({ "tool": "t", "args": {} });
        let id = svc.create(None, None, &op).await.unwrap();
        svc.decide(&id, "u1", "approved", None, Some(&op)).await.unwrap();
        // A second decision is rejected (already decided).
        assert!(svc.decide(&id, "u1", "rejected", None, Some(&op)).await.is_err());
    }
}