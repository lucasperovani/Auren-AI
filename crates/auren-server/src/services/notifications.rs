//! Notification service (spec section 9). Persists notifications and fans
//! them out on the live event bus.

use serde_json::json;
use sqlx::SqlitePool;

use crate::{events::EventEnvelope, state::AppState};

pub struct NotificationService {
    db: SqlitePool,
    events: crate::events::SharedEventBus,
}

impl NotificationService {
    pub fn new(state: &AppState) -> Self {
        Self { db: state.db.clone(), events: state.events.clone() }
    }

    /// Create a notification for a user and broadcast `notification.created`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        user_id: &str,
        notification_type: &str,
        title: &str,
        body: &str,
        task_id: Option<&str>,
        task_run_id: Option<&str>,
        approval_request_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO notifications (id, user_id, task_id, task_run_id, approval_request_id, notification_type, title, body, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(task_id)
        .bind(task_run_id)
        .bind(approval_request_id)
        .bind(notification_type)
        .bind(title)
        .bind(body)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.db)
        .await?;

        self.events.publish(EventEnvelope::new(
            "notification.created",
            json!({
                "notification_id": id,
                "user_id": user_id,
                "notification_type": notification_type,
                "task_id": task_id,
                "task_run_id": task_run_id,
                "approval_request_id": approval_request_id,
            }),
        ));
        Ok(id)
    }
}