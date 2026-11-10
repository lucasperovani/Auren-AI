//! Task collaboration surface: notes on a card, references between cards, and
//! the activity timeline shown when a task is opened.
//!
//! Notes and links are shared by the user and the agent, so a card holds the
//! running context of the work: what was decided, what blocks it, and what the
//! agent actually did.

use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::{error::ApiError, state::AppState};

/// Link kinds. `blocks` carries ordering semantics (the target must finish
/// first); `relates` is informational.
pub const LINK_KINDS: [&str; 2] = ["blocks", "relates"];

pub struct TaskCollabService {
    db: SqlitePool,
}

impl TaskCollabService {
    pub fn new(state: &AppState) -> Self {
        Self { db: state.db.clone() }
    }

    /// Append a note to a card. `author` is `user` or `agent`.
    pub async fn add_note(
        &self,
        user_id: &str,
        task_id: &str,
        author: &str,
        body: &str,
    ) -> Result<String, ApiError> {
        if author != "user" && author != "agent" {
            return Err(ApiError::Unprocessable(
                "author must be 'user' or 'agent'".into(),
            ));
        }
        let body = body.trim();
        if body.is_empty() {
            return Err(ApiError::Unprocessable("note body is required".into()));
        }
        if body.chars().count() > 8_000 {
            return Err(ApiError::Unprocessable(
                "a note must be at most 8000 characters".into(),
            ));
        }
        require_owned_task(&self.db, user_id, task_id).await?;

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_notes (id, task_id, author, body, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(task_id)
        .bind(author)
        .bind(body)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.db)
        .await?;
        Ok(id)
    }

    /// Notes on a card, oldest first.
    pub async fn list_notes(
        &self,
        user_id: &str,
        task_id: &str,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
        require_owned_task(&self.db, user_id, task_id).await?;
        let rows = sqlx::query(
            "SELECT id, author, body, created_at FROM task_notes
             WHERE task_id = ? ORDER BY created_at",
        )
        .bind(task_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<String, _>("id"),
                    "author": r.get::<String, _>("author"),
                    "body": r.get::<String, _>("body"),
                    "created_at": r.get::<String, _>("created_at"),
                })
            })
            .collect())
    }

    pub async fn delete_note(
        &self,
        user_id: &str,
        task_id: &str,
        note_id: &str,
    ) -> Result<(), ApiError> {
        require_owned_task(&self.db, user_id, task_id).await?;
        let result = sqlx::query("DELETE FROM task_notes WHERE id = ? AND task_id = ?")
            .bind(note_id)
            .bind(task_id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    /// Link two cards. Both must belong to the user, and self-links are
    /// rejected by the schema.
    pub async fn link(
        &self,
        user_id: &str,
        from_task_id: &str,
        to_task_id: &str,
        kind: &str,
    ) -> Result<String, ApiError> {
        if !LINK_KINDS.contains(&kind) {
            return Err(ApiError::Unprocessable(format!(
                "unknown link kind '{kind}'; expected one of {}",
                LINK_KINDS.join(", ")
            )));
        }
        if from_task_id == to_task_id {
            return Err(ApiError::Unprocessable(
                "a task cannot reference itself".into(),
            ));
        }
        require_owned_task(&self.db, user_id, from_task_id).await?;
        require_owned_task(&self.db, user_id, to_task_id).await?;

        // Linking is idempotent: re-adding the same pair returns the existing
        // link instead of failing, which keeps agent retries harmless.
        if let Some(existing) = sqlx::query(
            "SELECT id FROM task_links WHERE from_task_id = ? AND to_task_id = ? AND kind = ?",
        )
        .bind(from_task_id)
        .bind(to_task_id)
        .bind(kind)
        .fetch_optional(&self.db)
        .await?
        {
            return Ok(existing.get("id"));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO task_links (id, from_task_id, to_task_id, kind, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(from_task_id)
        .bind(to_task_id)
        .bind(kind)
        .bind(&now)
        .execute(&self.db)
        .await
        .map_err(|e| {
            // The CHECK constraint also guards self-links; surface it clearly.
            if e.to_string().contains("CHECK") {
                ApiError::Unprocessable("a task cannot reference itself".into())
            } else {
                ApiError::from(e)
            }
        })?;
        Ok(id)
    }

    pub async fn unlink(&self, user_id: &str, task_id: &str, link_id: &str) -> Result<(), ApiError> {
        require_owned_task(&self.db, user_id, task_id).await?;
        let result = sqlx::query(
            "DELETE FROM task_links WHERE id = ? AND (from_task_id = ? OR to_task_id = ?)",
        )
        .bind(link_id)
        .bind(task_id)
        .bind(task_id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    /// Links touching a card, in both directions, with the other side's title
    /// and stage so the UI can render chips.
    pub async fn list_links(
        &self,
        user_id: &str,
        task_id: &str,
    ) -> Result<serde_json::Value, ApiError> {
        require_owned_task(&self.db, user_id, task_id).await?;
        let rows = sqlx::query(
            "SELECT l.id, l.kind, l.from_task_id, l.to_task_id,
                    f.title AS from_title, f.stage AS from_stage,
                    t.title AS to_title, t.stage AS to_stage
             FROM task_links l
             JOIN tasks f ON f.id = l.from_task_id
             JOIN tasks t ON t.id = l.to_task_id
             WHERE l.from_task_id = ? OR l.to_task_id = ?
             ORDER BY l.created_at",
        )
        .bind(task_id)
        .bind(task_id)
        .fetch_all(&self.db)
        .await?;

        let mut outgoing = Vec::new();
        let mut incoming = Vec::new();
        for r in &rows {
            let from: String = r.get("from_task_id");
            if from == task_id {
                outgoing.push(json!({
                    "id": r.get::<String, _>("id"),
                    "kind": r.get::<String, _>("kind"),
                    "task_id": r.get::<String, _>("to_task_id"),
                    "title": r.get::<String, _>("to_title"),
                    "stage": r.get::<String, _>("to_stage"),
                }));
            } else {
                incoming.push(json!({
                    "id": r.get::<String, _>("id"),
                    "kind": r.get::<String, _>("kind"),
                    "task_id": r.get::<String, _>("from_task_id"),
                    "title": r.get::<String, _>("from_title"),
                    "stage": r.get::<String, _>("from_stage"),
                }));
            }
        }
        Ok(json!({ "outgoing": outgoing, "incoming": incoming }))
    }

    /// Titles of the cards blocking `task_id` that are not done yet.
    ///
    /// A `blocks` link means "the source must finish before the target", so
    /// the blockers of a task are the *incoming* `blocks` links. Used to
    /// refuse assigning work that depends on unfinished cards.
    pub async fn open_blockers(
        &self,
        task_id: &str,
    ) -> Result<Vec<String>, ApiError> {
        let rows = sqlx::query(
            "SELECT f.title FROM task_links l
             JOIN tasks f ON f.id = l.from_task_id
             WHERE l.to_task_id = ? AND l.kind = 'blocks' AND f.stage != 'done'",
        )
        .bind(task_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.iter().map(|r| r.get("title")).collect())
    }

    /// Everything that happened on a task: notes and execution steps in one
    /// chronological timeline, so opening a card answers "what did the agent
    /// do here?".
    pub async fn activity(
        &self,
        user_id: &str,
        task_id: &str,
    ) -> Result<Vec<serde_json::Value>, ApiError> {
        require_owned_task(&self.db, user_id, task_id).await?;

        let mut items: Vec<(String, serde_json::Value)> = Vec::new();

        for note in self.list_notes(user_id, task_id).await? {
            let at = note["created_at"].as_str().unwrap_or_default().to_string();
            items.push((
                at,
                json!({
                    "kind": "note",
                    "author": note["author"],
                    "body": note["body"],
                    "created_at": note["created_at"],
                }),
            ));
        }

        // Runs and their steps: the concrete work performed.
        let runs = sqlx::query(
            "SELECT id, status, trigger, started_at, finished_at, created_at, error_message
             FROM task_runs WHERE task_id = ? ORDER BY created_at",
        )
        .bind(task_id)
        .fetch_all(&self.db)
        .await?;

        for run in &runs {
            let run_id: String = run.get("id");
            let at: String = run
                .try_get::<Option<String>, _>("started_at")
                .ok()
                .flatten()
                .unwrap_or_else(|| run.get("created_at"));
            items.push((
                at.clone(),
                json!({
                    "kind": "run",
                    "run_id": run_id,
                    "status": run.get::<String, _>("status"),
                    "trigger": run.get::<String, _>("trigger"),
                    "started_at": run.try_get::<Option<String>, _>("started_at").ok().flatten(),
                    "finished_at": run.try_get::<Option<String>, _>("finished_at").ok().flatten(),
                    "error_message": run.try_get::<Option<String>, _>("error_message").ok().flatten(),
                }),
            ));

            let steps = sqlx::query(
                "SELECT step_index, status, input_json, output_json, error_message, created_at
                 FROM task_steps WHERE task_run_id = ? ORDER BY step_index",
            )
            .bind(&run_id)
            .fetch_all(&self.db)
            .await?;

            for step in &steps {
                let input: String = step.get("input_json");
                let output: Option<String> = step.try_get("output_json").ok().flatten();
                items.push((
                    step.get::<String, _>("created_at"),
                    json!({
                        "kind": "step",
                        "run_id": run_id,
                        "step_index": step.get::<i64, _>("step_index"),
                        "status": step.get::<String, _>("status"),
                        "tool": serde_json::from_str::<serde_json::Value>(&input)
                            .ok()
                            .and_then(|v| v.get("tool").and_then(|t| t.as_str()).map(|s| s.to_string())),
                        "input": serde_json::from_str::<serde_json::Value>(&input).unwrap_or(json!({})),
                        "output": output
                            .as_deref()
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
                        "error_message": step.try_get::<Option<String>, _>("error_message").ok().flatten(),
                        "created_at": step.get::<String, _>("created_at"),
                    }),
                ));
            }
        }

        // Chronological: the timeline reads as a story.
        items.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(items.into_iter().map(|(_, value)| value).collect())
    }
}

/// Ownership check shared by every operation on a card.
async fn require_owned_task(db: &SqlitePool, user_id: &str, task_id: &str) -> Result<(), ApiError> {
    let owner: Option<String> = sqlx::query("SELECT user_id FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(db)
        .await?
        .and_then(|r| r.try_get("user_id").ok());
    if owner.as_deref() != Some(user_id) {
        return Err(ApiError::NotFound);
    }
    Ok(())
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
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&pool).await.unwrap();
        for (id, title, stage) in [("t1", "First", "todo"), ("t2", "Second", "analysis")] {
            sqlx::query("INSERT INTO tasks (id, user_id, agent_id, title, objective, status, stage, timezone, created_at, updated_at) VALUES (?, 'u1', 'ag1', ?, 'O', 'draft', ?, 'UTC', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
                .bind(id).bind(title).bind(stage)
                .execute(&pool).await.unwrap();
        }
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
    async fn notes_are_scoped_to_the_card() {
        let state = state().await;
        let svc = TaskCollabService::new(&state);
        svc.add_note("u1", "t1", "agent", "Planned the work").await.unwrap();
        svc.add_note("u1", "t1", "user", "Looks good").await.unwrap();

        assert_eq!(svc.list_notes("u1", "t1").await.unwrap().len(), 2);
        // Notes on one card never leak into another.
        assert!(svc.list_notes("u1", "t2").await.unwrap().is_empty());
        // Empty notes and unknown authors are rejected.
        assert!(svc.add_note("u1", "t1", "agent", "   ").await.is_err());
        assert!(svc.add_note("u1", "t1", "system", "x").await.is_err());
        // Another user cannot see the card at all.
        assert!(svc.list_notes("u2", "t1").await.is_err());
    }

    #[tokio::test]
    async fn links_are_bidirectional_and_idempotent() {
        let state = state().await;
        let svc = TaskCollabService::new(&state);
        svc.link("u1", "t1", "t2", "blocks").await.unwrap();
        // Re-linking returns the same link instead of failing.
        let first = svc.link("u1", "t1", "t2", "blocks").await.unwrap();
        let second = svc.link("u1", "t1", "t2", "blocks").await.unwrap();
        assert_eq!(first, second);

        // From the blocker's side the link is incoming; from the blocked side outgoing.
        let from_t1 = svc.list_links("u1", "t1").await.unwrap();
        assert_eq!(from_t1["outgoing"].as_array().unwrap().len(), 1);
        assert_eq!(from_t1["outgoing"][0]["title"], "Second");
        let from_t2 = svc.list_links("u1", "t2").await.unwrap();
        assert_eq!(from_t2["incoming"].as_array().unwrap().len(), 1);
        assert_eq!(from_t2["incoming"][0]["title"], "First");

        // `t1 blocks t2`, so t2 is blocked by t1 while t1 is not blocked.
        assert_eq!(svc.open_blockers("t2").await.unwrap(), vec!["First".to_string()]);
        assert!(svc.open_blockers("t1").await.unwrap().is_empty());
        // Finishing the blocker clears the block.
        sqlx::query("UPDATE tasks SET stage = 'done' WHERE id = 't1'")
            .execute(&state.db).await.unwrap();
        assert!(svc.open_blockers("t2").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_self_links_and_unknown_kinds() {
        let state = state().await;
        let svc = TaskCollabService::new(&state);
        assert!(svc.link("u1", "t1", "t1", "relates").await.is_err());
        assert!(svc.link("u1", "t1", "t2", "duplicates").await.is_err());
    }

    #[tokio::test]
    async fn activity_merges_notes_and_steps_in_order() {
        let state = state().await;
        let svc = TaskCollabService::new(&state);
        sqlx::query("INSERT INTO task_runs (id, task_id, status, trigger, started_at, created_at) VALUES ('r1','t1','completed','manual','2026-01-01T00:00:10Z','2026-01-01T00:00:10Z')")
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO task_steps (id, task_run_id, step_index, status, input_json, output_json, created_at) VALUES ('s1','r1',0,'completed','{\"tool\":\"file_read\",\"path\":\"a.txt\"}','{\"ok\":true}','2026-01-01T00:00:20Z')")
            .execute(&state.db).await.unwrap();
        sqlx::query("INSERT INTO task_notes (id, task_id, author, body, created_at) VALUES ('n1','t1','agent','Done reading','2026-01-01T00:00:30Z')")
            .execute(&state.db).await.unwrap();

        let activity = svc.activity("u1", "t1").await.unwrap();
        let kinds: Vec<&str> = activity.iter().filter_map(|a| a["kind"].as_str()).collect();
        assert_eq!(kinds, vec!["run", "step", "note"], "timeline is chronological");

        let step = activity.iter().find(|a| a["kind"] == "step").unwrap();
        assert_eq!(step["tool"], "file_read");
        assert_eq!(step["output"]["ok"], true);
    }
}
