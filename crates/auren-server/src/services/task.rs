//! Task service (spec sections 2.2 and 9). The single validated path for
//! creating and updating tasks — used by both the REST API and the
//! conversational agent.

use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::{error::ApiError, events::EventEnvelope, state::AppState};

pub struct TaskService {
    db: SqlitePool,
    events: crate::events::SharedEventBus,
    audit: super::audit::AuditService,
}

/// Validate a cron expression using the `cron` schedule subset supported by
/// the scheduler (5 fields: minute hour day-of-month month day-of-week).
fn validate_schedule(schedule: &serde_json::Value) -> Result<(), ApiError> {
    let expr = schedule
        .get("cron")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::Unprocessable("schedule must contain a 'cron' field".into()))?;
    if expr.split_whitespace().count() != 5 {
        return Err(ApiError::Unprocessable(
            "cron expression must have 5 fields (minute hour dom month dow)".into(),
        ));
    }
    Ok(())
}

impl TaskService {
    pub fn new(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            events: state.events.clone(),
            audit: super::audit::AuditService::new(state.db.clone()),
        }
    }

    /// Create a task. Enforces ownership, agent ownership, and schedule
    /// validation. Creates the persistent schedule transactionally.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        user_id: &str,
        agent_id: &str,
        title: &str,
        objective: &str,
        schedule: Option<&serde_json::Value>,
        timezone: &str,
        conversation_id: Option<&str>,
    ) -> Result<String, ApiError> {
        if title.trim().is_empty() || objective.trim().is_empty() {
            return Err(ApiError::Unprocessable("title and objective are required".into()));
        }
        if let Some(s) = schedule {
            validate_schedule(s)?;
        }

        // The agent must belong to the same user (ownership enforcement).
        let agent_owner: Option<String> =
            sqlx::query("SELECT user_id FROM agents WHERE id = ?")
                .bind(agent_id)
                .fetch_optional(&self.db)
                .await?
                .map(|r| r.get("user_id"));
        if agent_owner.as_deref() != Some(user_id) {
            return Err(ApiError::NotFound);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let status = if schedule.is_some() { "active" } else { "draft" };

        let mut tx = self.db.begin().await.map_err(ApiError::from)?;
        sqlx::query(
            "INSERT INTO tasks (id, user_id, agent_id, conversation_id, title, objective, status, schedule_json, timezone, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(agent_id)
        .bind(conversation_id)
        .bind(title.trim())
        .bind(objective.trim())
        .bind(status)
        .bind(schedule.map(|s| s.to_string()))
        .bind(timezone)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::from)?;

        if let Some(s) = schedule {
            self.upsert_schedule_in_tx(&mut tx, &id, s).await?;
        }
        tx.commit().await.map_err(ApiError::from)?;

        self.events.publish(EventEnvelope::new(
            "task.created",
            json!({ "task_id": id, "user_id": user_id }),
        ));
        self.audit
            .record(None, "task.created", super::audit::ActorType::User, Some(user_id), json!({ "task_id": id }))
            .await;
        Ok(id)
    }

    /// Insert or replace the persistent schedule for a task inside a
    /// transaction (uniqueness on `task_id` prevents duplicates).
    async fn upsert_schedule_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
        schedule: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let next_run = super::scheduler::next_run_at(schedule)
            .ok_or_else(|| ApiError::Unprocessable("schedule never fires".into()))?;
        sqlx::query(
            "INSERT INTO scheduled_jobs (id, task_id, next_run_at, schedule_json, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'active', ?, ?)
             ON CONFLICT(task_id) DO UPDATE SET
               next_run_at = excluded.next_run_at,
               schedule_json = excluded.schedule_json,
               status = 'active',
               updated_at = excluded.updated_at",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(next_run.to_rfc3339())
        .bind(schedule.to_string())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut **tx)
        .await
        .map_err(ApiError::from)?;
        Ok(())
    }

    /// Update task fields; revalidates the schedule when provided.
    pub async fn update(
        &self,
        user_id: &str,
        task_id: &str,
        title: Option<&str>,
        objective: Option<&str>,
        schedule: Option<&serde_json::Value>,
    ) -> Result<(), ApiError> {
        let owned = self.require_owned(user_id, task_id).await?;
        if owned == "completed" || owned == "cancelled" {
            return Err(ApiError::Conflict("task is closed".into()));
        }
        if let Some(s) = schedule {
            validate_schedule(s)?;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.db.begin().await.map_err(ApiError::from)?;
        if let Some(t) = title {
            sqlx::query("UPDATE tasks SET title = ?, updated_at = ? WHERE id = ?")
                .bind(t.trim())
                .bind(&now)
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(o) = objective {
            sqlx::query("UPDATE tasks SET objective = ?, updated_at = ? WHERE id = ?")
                .bind(o.trim())
                .bind(&now)
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(s) = schedule {
            sqlx::query("UPDATE tasks SET schedule_json = ?, status = 'active', updated_at = ? WHERE id = ?")
                .bind(s.to_string())
                .bind(&now)
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
            self.upsert_schedule_in_tx(&mut tx, task_id, s).await?;
        }
        tx.commit().await.map_err(ApiError::from)?;

        self.events.publish(EventEnvelope::new(
            "task.updated",
            json!({ "task_id": task_id }),
        ));
        Ok(())
    }

    /// Transition task status with validation, emitting the matching event.
    pub async fn set_status(
        &self,
        user_id: &str,
        task_id: &str,
        status: &str,
    ) -> Result<(), ApiError> {
        self.require_owned(user_id, task_id).await?;
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query("UPDATE tasks SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(&now)
            .bind(task_id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        if status == "paused" {
            sqlx::query("UPDATE scheduled_jobs SET status = 'paused', updated_at = ? WHERE task_id = ?")
                .bind(&now)
                .bind(task_id)
                .execute(&self.db)
                .await?;
        }
        if status == "active" {
            sqlx::query("UPDATE scheduled_jobs SET status = 'active', updated_at = ? WHERE task_id = ?")
                .bind(&now)
                .bind(task_id)
                .execute(&self.db)
                .await?;
        }
        let event_type = match status {
            "paused" => "task.paused",
            "cancelled" => "task.cancelled",
            _ => "task.updated",
        };
        self.events
            .publish(EventEnvelope::new(event_type, json!({ "task_id": task_id })));
        Ok(())
    }

    /// Request an immediate run: creates a run and enqueues it.
    pub async fn request_run(&self, user_id: &str, task_id: &str) -> Result<String, ApiError> {
        self.require_owned(user_id, task_id).await?;
        let run_id = super::scheduler::create_run_and_enqueue(&self.db, task_id, "manual")
            .await
            .map_err(ApiError::from)?;
        self.events.publish(EventEnvelope::new(
            "task.started",
            json!({ "task_id": task_id, "task_run_id": run_id }),
        ));
        Ok(run_id)
    }

    /// Fetch a task owned by the user or return 404 (ownership enforcement).
    pub async fn require_owned(&self, user_id: &str, task_id: &str) -> Result<String, ApiError> {
        let row = sqlx::query("SELECT user_id, status FROM tasks WHERE id = ?")
            .bind(task_id)
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let owner: String = row.get("user_id");
        if owner != user_id {
            return Err(ApiError::NotFound);
        }
        Ok(row.get("status"))
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
            },
            events: std::sync::Arc::new(crate::events::EventBus::new()),
            ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
        }
    }

    async fn seed_user_agent(s: &AppState) -> (String, String) {
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&s.db).await.unwrap();
        ("u1".into(), "ag1".into())
    }

    #[tokio::test]
    async fn create_task_with_schedule() {
        let s = state().await;
        let (user, agent) = seed_user_agent(&s).await;
        let svc = TaskService::new(&s);
        let schedule = json!({ "cron": "*/5 * * * *" });
        let id = svc.create(&user, &agent, "T", "O", Some(&schedule), "UTC", None).await.unwrap();
        let row = sqlx::query("SELECT status FROM tasks WHERE id = ?").bind(&id).fetch_one(&s.db).await.unwrap();
        assert_eq!(row.get::<String, _>("status"), "active");
        let job = sqlx::query("SELECT status FROM scheduled_jobs WHERE task_id = ?").bind(&id).fetch_one(&s.db).await.unwrap();
        assert_eq!(job.get::<String, _>("status"), "active");
    }

    #[tokio::test]
    async fn rejects_invalid_schedule() {
        let s = state().await;
        let (user, agent) = seed_user_agent(&s).await;
        let svc = TaskService::new(&s);
        let schedule = json!({ "cron": "not a cron" });
        assert!(svc.create(&user, &agent, "T", "O", Some(&schedule), "UTC", None).await.is_err());
    }

    #[tokio::test]
    async fn enforces_ownership() {
        let s = state().await;
        let (user, agent) = seed_user_agent(&s).await;
        let svc = TaskService::new(&s);
        let id = svc.create(&user, &agent, "T", "O", None, "UTC", None).await.unwrap();
        // Another user cannot see or run the task.
        assert!(svc.require_owned("u2", &id).await.is_err());
        assert!(svc.request_run("u2", &id).await.is_err());
        // The owner can.
        assert!(svc.request_run(&user, &id).await.is_ok());
    }
}