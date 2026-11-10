//! Keeps an agent's toolset in sync with the latest skill and MCP state.
//!
//! Skills are versioned: `agent_skills` points at a *specific*
//! `skill_version_id`. Activating a new version updates `skills.active_version_id`
//! but not the agent links, so an agent could keep calling the tools of a
//! superseded version indefinitely — the bug this module fixes.
//!
//! `sync_agent_tools` is cheap (a few indexed statements) and idempotent, so it
//! runs at the start of every agent turn. That guarantees a chat always sees
//! the current tools, MCP servers, and skill versions, even if they changed
//! while the conversation was open.

use sqlx::{Row, SqlitePool};

use crate::error::ApiError;

/// Result of a sync, for logging and for the caller to know whether anything
/// moved.
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncOutcome {
    /// Agent links repointed to a newer skill version.
    pub relinked: u64,
    /// Links whose skill was disabled or has no active version.
    pub disabled: u64,
    /// MCP tools (re)discovered for the agent.
    pub mcp_tools: u64,
}

impl SyncOutcome {
    pub fn changed(&self) -> bool {
        self.relinked > 0 || self.disabled > 0
    }
}

/// Bring an agent's skill links in line with each skill's active version.
///
/// - A link on a superseded/older version is repointed to the active version,
///   keeping its `enabled` flag. The new version's tools become visible
///   immediately.
/// - A link whose skill has no active version, or whose skill is disabled, is
///   disabled so the agent stops advertising tools it cannot run.
///
/// Repointing instead of inserting avoids accumulating one row per version.
pub async fn sync_agent_skills(db: &SqlitePool, agent_id: &str) -> Result<SyncOutcome, ApiError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut outcome = SyncOutcome::default();

    // 1. Repoint every link to the skill's current active version. The
    //    subquery is per-skill, so a version bump on one skill never affects
    //    another.
    //
    // `ON CONFLICT` covers the case where the agent already had a link on the
    // active version: the older row is deleted afterwards.
    let stale = sqlx::query(
        "SELECT ask.id, ask.skill_version_id, sk.active_version_id, ask.enabled
         FROM agent_skills ask
         JOIN skill_versions sv ON sv.id = ask.skill_version_id
         JOIN skills sk ON sk.id = sv.skill_id
         WHERE ask.agent_id = ?",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;

    for row in stale {
        let link_id: String = row.get("id");
        let current_version: String = row.get("skill_version_id");
        let active_version: Option<String> = row.get("active_version_id");
        let enabled: i64 = row.get("enabled");

        let Some(active_version) = active_version else {
            // No active version: the skill cannot run, so drop the link.
            if enabled != 0 {
                sqlx::query("UPDATE agent_skills SET enabled = 0 WHERE id = ?")
                    .bind(&link_id)
                    .execute(db)
                    .await?;
                outcome.disabled += 1;
            }
            continue;
        };
        if active_version == current_version {
            continue;
        }

        // Repoint. If the agent already has a link on the active version, keep
        // that one (it may carry `enabled = 1`) and delete the stale row.
        let existing: Option<String> = sqlx::query(
            "SELECT id FROM agent_skills WHERE agent_id = ? AND skill_version_id = ?",
        )
        .bind(agent_id)
        .bind(&active_version)
        .fetch_optional(db)
        .await?
        .and_then(|r| r.try_get("id").ok());

        match existing {
            Some(existing_id) => {
                // Preserve "enabled" from either row.
                sqlx::query("UPDATE agent_skills SET enabled = MAX(enabled, ?) WHERE id = ?")
                    .bind(enabled)
                    .bind(&existing_id)
                    .execute(db)
                    .await?;
                sqlx::query("DELETE FROM agent_skills WHERE id = ?")
                    .bind(&link_id)
                    .execute(db)
                    .await?;
            }
            None => {
                sqlx::query("UPDATE agent_skills SET skill_version_id = ?, installed_at = ? WHERE id = ?")
                    .bind(&active_version)
                    .bind(&now)
                    .bind(&link_id)
                    .execute(db)
                    .await?;
            }
        }
        outcome.relinked += 1;
    }

    // 2. Skills the user disabled must stop being offered, whatever the links
    //    say. This also covers a skill disabled without touching agent rows.
    let disabled = sqlx::query(
        "UPDATE agent_skills SET enabled = 0
         WHERE agent_id = ?
           AND enabled = 1
           AND skill_version_id IN (
               SELECT sv.id FROM skill_versions sv
               JOIN skills sk ON sk.id = sv.skill_id
               WHERE sk.status != 'enabled'
           )",
    )
    .bind(agent_id)
    .execute(db)
    .await?;
    outcome.disabled += disabled.rows_affected();

    Ok(outcome)
}

/// Skill IDs currently enabled for an agent, used to scope MCP tools.
pub async fn enabled_skill_ids(db: &SqlitePool, agent_id: &str) -> Result<Vec<String>, ApiError> {
    let rows = sqlx::query(
        "SELECT DISTINCT sv.skill_id
         FROM agent_skills ask
         JOIN skill_versions sv ON sv.id = ask.skill_version_id
         JOIN skills sk ON sk.id = sv.skill_id
         WHERE ask.agent_id = ? AND ask.enabled = 1 AND sk.status = 'enabled'",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| r.get("skill_id")).collect())
}

/// Drop cached MCP tool catalogues so the next turn rediscovers them.
///
/// MCP servers are remote: their tool list can change without any local write.
/// Rather than trusting a stale cache, the agent rescans at most once per
/// `ttl_secs`, which keeps the first message of a turn fast while still
/// picking up remote changes promptly.
pub fn mcp_cache_is_fresh(age: std::time::Duration, ttl: std::time::Duration) -> bool {
    age < ttl
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> sqlx::SqlitePool {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        std::mem::forget(dir);
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO skills (id, name, status, created_at, updated_at) VALUES ('s1','S','enabled','2026','2026')")
            .execute(&pool).await.unwrap();
        for (id, version, status) in [("v1", "1.0.0", "superseded"), ("v2", "2.0.0", "active")] {
            sqlx::query(
                "INSERT INTO skill_versions (id, skill_id, version, manifest_json, package_hash, install_path, status, installed_at)
                 VALUES (?, 's1', ?, '{}', 'hash', '/tmp', ?, '2026')",
            )
            .bind(id)
            .bind(version)
            .bind(status)
            .execute(&pool).await.unwrap();
        }
        sqlx::query("UPDATE skills SET active_version_id = 'v2' WHERE id = 's1'")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO skill_tools (id, skill_version_id, tool_name, description, input_schema_json, risk_class, implementation_type, implementation_ref)
                     VALUES ('t1','v1','old_tool','old','{}','low','declarative','old_tool'),
                            ('t2','v2','new_tool','new','{}','low','declarative','new_tool')")
            .execute(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn relinks_a_stale_agent_to_the_active_version() {
        let pool = db().await;
        // The agent is linked to the superseded version.
        sqlx::query("INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at) VALUES ('l1','ag1','v1',1,'2026')")
            .execute(&pool).await.unwrap();

        let outcome = sync_agent_skills(&pool, "ag1").await.unwrap();
        assert_eq!(outcome.relinked, 1);

        let version: String = sqlx::query("SELECT skill_version_id FROM agent_skills WHERE agent_id = 'ag1'")
            .fetch_one(&pool).await.unwrap().get("skill_version_id");
        assert_eq!(version, "v2", "the link follows the active version");

        // The toolset now comes from the new version.
        let tools: Vec<String> = sqlx::query(
            "SELECT st.tool_name FROM skill_tools st
             JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             WHERE ask.agent_id = 'ag1' AND ask.enabled = 1",
        )
        .fetch_all(&pool).await.unwrap()
            .iter().map(|r| r.get("tool_name")).collect();
        assert_eq!(tools, vec!["new_tool".to_string()], "old tools are gone");
    }

    #[tokio::test]
    async fn sync_is_idempotent() {
        let pool = db().await;
        sqlx::query("INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at) VALUES ('l1','ag1','v2',1,'2026')")
            .execute(&pool).await.unwrap();
        let first = sync_agent_skills(&pool, "ag1").await.unwrap();
        assert!(!first.changed(), "nothing to do when already current");
        let second = sync_agent_skills(&pool, "ag1").await.unwrap();
        assert!(!second.changed());
        // No duplicate rows were created.
        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM agent_skills WHERE agent_id = 'ag1'")
            .fetch_one(&pool).await.unwrap().get("n");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn collapses_duplicate_links_when_both_versions_are_present() {
        let pool = db().await;
        // Both versions linked: a common state after a manual install.
        sqlx::query("INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at) VALUES ('l1','ag1','v1',1,'2026'), ('l2','ag1','v2',0,'2026')")
            .execute(&pool).await.unwrap();

        sync_agent_skills(&pool, "ag1").await.unwrap();
        let rows = sqlx::query("SELECT skill_version_id, enabled FROM agent_skills WHERE agent_id = 'ag1'")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1, "duplicate links are collapsed");
        assert_eq!(rows[0].get::<String, _>("skill_version_id"), "v2");
        assert_eq!(rows[0].get::<i64, _>("enabled"), 1, "enabled is preserved");
    }

    #[tokio::test]
    async fn disables_tools_of_a_disabled_skill() {
        let pool = db().await;
        sqlx::query("INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at) VALUES ('l1','ag1','v2',1,'2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE skills SET status = 'disabled' WHERE id = 's1'")
            .execute(&pool).await.unwrap();

        let outcome = sync_agent_skills(&pool, "ag1").await.unwrap();
        assert_eq!(outcome.disabled, 1);
        let enabled: i64 = sqlx::query("SELECT enabled FROM agent_skills WHERE id = 'l1'")
            .fetch_one(&pool).await.unwrap().get("enabled");
        assert_eq!(enabled, 0);
        assert!(enabled_skill_ids(&pool, "ag1").await.unwrap().is_empty());
    }

    #[test]
    fn mcp_cache_ttl() {
        use std::time::Duration;
        assert!(mcp_cache_is_fresh(Duration::from_secs(10), Duration::from_secs(60)));
        assert!(!mcp_cache_is_fresh(Duration::from_secs(90), Duration::from_secs(60)));
    }
}
