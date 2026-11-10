//! Notification service (spec section 9). Persists notifications and fans
//! them out on the live event bus.
//!
//! Notifications are the user-facing inbox: they carry a title, a body, a
//! severity, and optional actions. An action either hands the work back to the
//! agent (so it acts on the notification) or performs a local decision.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::{events::EventEnvelope, state::AppState};

/// A button shown on a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationAction {
    /// Stable identifier echoed back when the action is chosen.
    pub id: String,
    /// Button label.
    pub label: String,
    /// Visual style: `primary`, `danger`, or empty for default.
    #[serde(default)]
    pub style: String,
    /// `agent` hands the payload to the agent; `dismiss` only closes it.
    pub kind: String,
    /// Arguments for the action, e.g. the prompt the agent should run.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Extra fields when creating a notification.
#[derive(Debug, Clone)]
pub struct NotificationOptions {
    /// `info`, `success`, `warning`, or `error`.
    pub severity: &'static str,
    pub actions: Vec<NotificationAction>,
}

impl Default for NotificationOptions {
    fn default() -> Self {
        Self { severity: "info", actions: Vec::new() }
    }
}

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
        self.create_full(
            user_id,
            notification_type,
            title,
            body,
            task_id,
            task_run_id,
            approval_request_id,
            NotificationOptions::default(),
        )
        .await
    }

    /// Create a notification with a severity and action buttons.
    pub async fn create_with_actions(
        &self,
        user_id: &str,
        notification_type: &str,
        title: &str,
        body: &str,
        options: NotificationOptions,
    ) -> anyhow::Result<String> {
        self.create_full(user_id, notification_type, title, body, None, None, None, options)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_full(
        &self,
        user_id: &str,
        notification_type: &str,
        title: &str,
        body: &str,
        task_id: Option<&str>,
        task_run_id: Option<&str>,
        approval_request_id: Option<&str>,
        options: NotificationOptions,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let actions_json = serde_json::to_string(&options.actions).unwrap_or_else(|_| "[]".into());
        sqlx::query(
            "INSERT INTO notifications (id, user_id, task_id, task_run_id, approval_request_id, notification_type, title, body, actions_json, severity, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(task_id)
        .bind(task_run_id)
        .bind(approval_request_id)
        .bind(notification_type)
        .bind(title)
        .bind(body)
        .bind(&actions_json)
        .bind(options.severity)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.db)
        .await?;

        self.events.publish(EventEnvelope::new(
            "notification.created",
            json!({
                "notification_id": id,
                "user_id": user_id,
                "notification_type": notification_type,
                "severity": options.severity,
                "task_id": task_id,
                "task_run_id": task_run_id,
                "approval_request_id": approval_request_id,
            }),
        ));
        Ok(id)
    }

    /// Record which action the user chose and when.
    pub async fn record_action(
        &self,
        user_id: &str,
        notification_id: &str,
        action_id: &str,
    ) -> anyhow::Result<Option<NotificationAction>> {
        let row = sqlx::query(
            "SELECT actions_json FROM notifications WHERE id = ? AND user_id = ?",
        )
        .bind(notification_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let raw: String = row.get("actions_json");
        let actions: Vec<NotificationAction> = serde_json::from_str(&raw).unwrap_or_default();
        let Some(action) = actions.into_iter().find(|a| a.id == action_id) else {
            return Ok(None);
        };

        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE notifications SET action_id = ?, acted_at = ?, read_at = COALESCE(read_at, ?) WHERE id = ? AND user_id = ?",
        )
        .bind(&action.id)
        .bind(&now)
        .bind(&now)
        .bind(notification_id)
        .bind(user_id)
        .execute(&self.db)
        .await?;

        self.events.publish(EventEnvelope::new(
            "notification.acted",
            json!({ "notification_id": notification_id, "action_id": action.id }),
        ));
        Ok(Some(action))
    }
}
