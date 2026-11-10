//! Projects: the unit that keeps work separated.
//!
//! A project groups tasks so that boards, listings, and the agent's view of
//! the world are scoped. Tasks without a project form the default board, which
//! keeps behavior unchanged for callers that never create projects.

use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::{error::ApiError, state::AppState};

/// Allowed accent colors; kept in sync with the DB CHECK constraint.
pub const PROJECT_COLORS: [&str; 6] = ["blue", "green", "purple", "orange", "pink", "gray"];

pub struct ProjectService {
    db: SqlitePool,
}

impl ProjectService {
    pub fn new(state: &AppState) -> Self {
        Self { db: state.db.clone() }
    }

    /// Create a project. Names are unique per user so the agent can refer to a
    /// project by name without ambiguity.
    pub async fn create(
        &self,
        user_id: &str,
        name: &str,
        description: &str,
        color: Option<&str>,
    ) -> Result<String, ApiError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(ApiError::Unprocessable("project name is required".into()));
        }
        if name.chars().count() > 80 {
            return Err(ApiError::Unprocessable(
                "project name must be at most 80 characters".into(),
            ));
        }
        let color = color.unwrap_or("blue");
        if !PROJECT_COLORS.contains(&color) {
            return Err(ApiError::Unprocessable(format!(
                "unknown color '{color}'; expected one of {}",
                PROJECT_COLORS.join(", ")
            )));
        }

        let duplicate: Option<String> = sqlx::query(
            "SELECT id FROM projects WHERE user_id = ? AND name = ? COLLATE NOCASE",
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?
        .and_then(|r| r.try_get("id").ok());
        if duplicate.is_some() {
            return Err(ApiError::Conflict(format!(
                "a project named '{name}' already exists"
            )));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO projects (id, user_id, name, description, color, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, 'active', ?, ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(name)
        .bind(description.trim())
        .bind(color)
        .bind(&now)
        .bind(&now)
        .execute(&self.db)
        .await?;
        Ok(id)
    }

    /// Projects owned by the user with their task counts, for the board
    /// switcher.
    pub async fn list(&self, user_id: &str) -> Result<Vec<serde_json::Value>, ApiError> {
        let rows = sqlx::query(
            "SELECT p.id, p.name, p.description, p.color, p.status, p.created_at,
                    (SELECT COUNT(*) FROM tasks t WHERE t.project_id = p.id) AS task_count,
                    (SELECT COUNT(*) FROM tasks t WHERE t.project_id = p.id AND t.stage = 'in_progress') AS active_count
             FROM projects p
             WHERE p.user_id = ? AND p.status = 'active'
             ORDER BY p.created_at",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<String, _>("id"),
                    "name": r.get::<String, _>("name"),
                    "description": r.get::<String, _>("description"),
                    "color": r.get::<String, _>("color"),
                    "status": r.get::<String, _>("status"),
                    "task_count": r.get::<i64, _>("task_count"),
                    "active_count": r.get::<i64, _>("active_count"),
                    "created_at": r.get::<String, _>("created_at"),
                })
            })
            .collect())
    }

    /// Resolve a project the user owns, by id. `None` means the default board.
    pub async fn require_owned(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<(), ApiError> {
        let owner: Option<String> = sqlx::query("SELECT user_id FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(&self.db)
            .await?
            .and_then(|r| r.try_get("user_id").ok());
        if owner.as_deref() != Some(user_id) {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    /// Resolve a project by name or id, so the agent can say "the Q4 project".
    pub async fn resolve(&self, user_id: &str, reference: &str) -> Result<Option<String>, ApiError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT id FROM projects
             WHERE user_id = ? AND status = 'active' AND (id = ? OR name = ? COLLATE NOCASE)
             LIMIT 1",
        )
        .bind(user_id)
        .bind(reference)
        .bind(reference)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.and_then(|r| r.try_get("id").ok()))
    }

    /// Update name, description, color, or archive status.
    pub async fn update(
        &self,
        user_id: &str,
        project_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        color: Option<&str>,
        archived: Option<bool>,
    ) -> Result<(), ApiError> {
        self.require_owned(user_id, project_id).await?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(name) = name {
            let name = name.trim();
            if name.is_empty() {
                return Err(ApiError::Unprocessable("project name is required".into()));
            }
            sqlx::query("UPDATE projects SET name = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(&now)
                .bind(project_id)
                .execute(&self.db)
                .await?;
        }
        if let Some(description) = description {
            sqlx::query("UPDATE projects SET description = ?, updated_at = ? WHERE id = ?")
                .bind(description.trim())
                .bind(&now)
                .bind(project_id)
                .execute(&self.db)
                .await?;
        }
        if let Some(color) = color {
            if !PROJECT_COLORS.contains(&color) {
                return Err(ApiError::Unprocessable(format!(
                    "unknown color '{color}'; expected one of {}",
                    PROJECT_COLORS.join(", ")
                )));
            }
            sqlx::query("UPDATE projects SET color = ?, updated_at = ? WHERE id = ?")
                .bind(color)
                .bind(&now)
                .bind(project_id)
                .execute(&self.db)
                .await?;
        }
        if let Some(archived) = archived {
            let status = if archived { "archived" } else { "active" };
            sqlx::query("UPDATE projects SET status = ?, updated_at = ? WHERE id = ?")
                .bind(status)
                .bind(&now)
                .bind(project_id)
                .execute(&self.db)
                .await?;
        }
        Ok(())
    }

    /// Delete a project. Tasks are detached, never deleted: losing work
    /// because a container was removed would be destructive.
    pub async fn delete(&self, user_id: &str, project_id: &str) -> Result<(), ApiError> {
        self.require_owned(user_id, project_id).await?;
        let mut tx = self.db.begin().await.map_err(ApiError::from)?;
        sqlx::query("UPDATE tasks SET project_id = NULL WHERE project_id = ?")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await.map_err(ApiError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        std::mem::forget(dir);
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&pool).await.unwrap();
        AppState {
            db: pool,
            config: crate::config::AppConfig {
                database_url: url,
                bind_addr: "127.0.0.1:0".into(),
                skills_dir: "data/skills".into(),
                max_upload_bytes: 1024,
                session_secret: "s".into(),
                workspace_dir: "data/workspace".into(),
                max_workspace_file_bytes: 1024,
                credential_encryption_key: "k".into(),
                max_iterations: 25,
                max_tool_calls: 50,
                max_llm_calls: 40,
                subagent_auto_delete: false,
            },
            events: std::sync::Arc::new(crate::events::EventBus::new()),
            ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
        }
    }

    #[tokio::test]
    async fn project_names_are_unique_per_user() {
        let state = state().await;
        let svc = ProjectService::new(&state);
        svc.create("u1", "Website", "", None).await.unwrap();
        // Case-insensitive duplicate is rejected so the agent can resolve by name.
        let err = svc.create("u1", "website", "", None).await.unwrap_err();
        assert!(matches!(err, ApiError::Conflict(_)));
        // A different name is fine.
        svc.create("u1", "Marketing", "", Some("green")).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_color_and_empty_name() {
        let state = state().await;
        let svc = ProjectService::new(&state);
        assert!(svc.create("u1", "  ", "", None).await.is_err());
        assert!(svc.create("u1", "X", "", Some("neon")).await.is_err());
    }

    #[tokio::test]
    async fn resolves_by_id_and_name() {
        let state = state().await;
        let svc = ProjectService::new(&state);
        let id = svc.create("u1", "Q4 Report", "", None).await.unwrap();
        assert_eq!(svc.resolve("u1", &id).await.unwrap(), Some(id.clone()));
        assert_eq!(svc.resolve("u1", "q4 report").await.unwrap(), Some(id.clone()));
        assert_eq!(svc.resolve("u1", "nope").await.unwrap(), None);
        // Another user cannot see it.
        assert_eq!(svc.resolve("u2", "Q4 Report").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_detaches_tasks_instead_of_removing_them() {
        let state = state().await;
        let svc = ProjectService::new(&state);
        let id = svc.create("u1", "Temp", "", None).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO tasks (id, user_id, agent_id, title, objective, status, stage, project_id, timezone, created_at, updated_at) VALUES ('t1','u1','ag1','T','O','draft','todo',?,'UTC','2026','2026')")
            .bind(&id)
            .execute(&state.db).await.unwrap();

        svc.delete("u1", &id).await.unwrap();
        let project: Option<String> = sqlx::query("SELECT project_id FROM tasks WHERE id = 't1'")
            .fetch_one(&state.db).await.unwrap().get("project_id");
        assert!(project.is_none(), "the task survives, detached from the project");
        let exists: i64 = sqlx::query("SELECT COUNT(*) AS n FROM projects WHERE id = ?")
            .bind(&id).fetch_one(&state.db).await.unwrap().get("n");
        assert_eq!(exists, 0);
    }
}
