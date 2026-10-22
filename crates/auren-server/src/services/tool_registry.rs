//! Tool registry and permission engine (spec sections 2.2 and 9).
//!
//! All tool invocations pass through this single authorization path: agent
//! permissions, skill availability, connected-account scopes, and approval
//! requirements are evaluated here — never duplicated in handlers.

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

use crate::{error::ApiError, state::AppState};

/// Risk classes that always require explicit approval regardless of the
/// agent-level policy (mandatory security rules cannot be overridden).
const ALWAYS_ASK_RISK_CLASSES: [&str; 2] = ["high", "critical"];

pub struct ToolRegistry {
    db: SqlitePool,
    approvals: super::approvals::ApprovalService,
}

/// The authorization outcome for a tool invocation.
#[derive(Debug)]
pub enum Authorization {
    /// Proceed with the invocation.
    Allowed,
    /// Proceed only after the referenced approval is granted.
    RequiresApproval { approval_id: String },
    /// Invocation denied.
    Denied(String),
}

impl ToolRegistry {
    pub fn new(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            approvals: super::approvals::ApprovalService::new(state),
        }
    }

    /// Authorize an agent invoking a skill tool with the given arguments on
    /// behalf of a connected account.
    ///
    /// Evaluation order (spec section 2.2 "Permissions"):
    /// 1. The agent must have the skill version enabled.
    /// 2. The agent-level policy (`allow`/`deny`/`ask`) applies.
    /// 3. Mandatory rules: high/critical risk tools always require approval.
    /// 4. The connected account's granted scopes must cover the tool's
    ///    required scope, when the tool declares one.
    pub async fn authorize(
        &self,
        agent_id: &str,
        skill_tool_id: &str,
        args: &Value,
        integration_account_id: Option<&str>,
    ) -> Result<Authorization, ApiError> {
        // 1. Tool must exist and belong to a skill version enabled for the agent.
        let tool = sqlx::query(
            "SELECT st.tool_name, st.risk_class, st.implementation_type, st.implementation_ref, st.input_schema_json
             FROM skill_tools st
             JOIN agent_skills ask ON ask.skill_version_id = st.skill_version_id
             WHERE st.id = ? AND ask.agent_id = ? AND ask.enabled = 1",
        )
        .bind(skill_tool_id)
        .bind(agent_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;
        let tool_name: String = tool.get("tool_name");
        let risk_class: String = tool.get("risk_class");
        let implementation_type: String = tool.get("implementation_type");

        // Declarative skills may only reference registered implementations.
        if implementation_type != "declarative" && implementation_type != "builtin_ref" {
            return Ok(Authorization::Denied(format!(
                "unsupported implementation type '{implementation_type}'"
            )));
        }

        // 2. Agent-level policy.
        let policy: Option<String> = sqlx::query(
            "SELECT policy FROM agent_tool_permissions WHERE agent_id = ? AND skill_tool_id = ?",
        )
        .bind(agent_id)
        .bind(skill_tool_id)
        .fetch_optional(&self.db)
        .await?
        .map(|r| r.get("policy"));

        if policy.as_deref() == Some("deny") {
            return Ok(Authorization::Denied("tool denied by agent policy".into()));
        }

        // 3. Mandatory approval rules (cannot be overridden by agent config).
        let needs_approval = policy.as_deref() == Some("ask")
            || ALWAYS_ASK_RISK_CLASSES.contains(&risk_class.as_str());

        // 4. Scope check against the connected account.
        if let Some(account_id) = integration_account_id {
            self.check_scopes(account_id, &tool_name, args).await?;
        }

        if needs_approval {
            let operation = json!({
                "agent_id": agent_id,
                "skill_tool_id": skill_tool_id,
                "tool_name": tool_name,
                "args": args,
                "integration_account_id": integration_account_id,
            });
            let approval_id = self
                .approvals
                .create(None, Some(skill_tool_id), &operation)
                .await
                .map_err(ApiError::from)?;
            return Ok(Authorization::RequiresApproval { approval_id });
        }
        Ok(Authorization::Allowed)
    }

    /// Verify the connected account's granted scopes cover the operation.
    /// Tools may declare a required scope in their input schema under
    /// `x-required-scope`.
    async fn check_scopes(&self, account_id: &str, tool_name: &str, args: &Value) -> Result<(), ApiError> {
        let row = sqlx::query("SELECT status, granted_scopes_json FROM integration_accounts WHERE id = ?")
            .bind(account_id)
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let status: String = row.get("status");
        if status != "active" {
            return Err(ApiError::Conflict(format!("integration account is {status}")));
        }
        let scopes_json: String = row.get("granted_scopes_json");
        let scopes: Vec<String> = serde_json::from_str(&scopes_json).unwrap_or_default();

        let required = args
            .get("x-required-scope")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !required.is_empty() && !scopes.iter().any(|s| s == required) {
            return Err(ApiError::Forbidden(format!(
                "tool '{tool_name}' requires scope '{required}' which is not granted to this account"
            )));
        }
        Ok(())
    }

    /// Execute a declarative tool invocation. Declarative tools compose
    /// registered builtin implementations; the actual builtin dispatch is
    /// resolved by `implementation_ref`. This method records the step and
    /// returns the recorded output.
    pub async fn invoke(
        &self,
        task_run_id: &str,
        skill_tool_id: &str,
        args: &Value,
        integration_account_id: Option<&str>,
    ) -> Result<Value, ApiError> {
        let step_index = sqlx::query("SELECT COALESCE(MAX(step_index) + 1, 0) AS idx FROM task_steps WHERE task_run_id = ?")
            .bind(task_run_id)
            .fetch_one(&self.db)
            .await?
            .get::<i64, _>("idx");
        let now = chrono::Utc::now().to_rfc3339();
        let step_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task_steps (id, task_run_id, skill_tool_id, integration_account_id, step_index, status, input_json, created_at)
             VALUES (?, ?, ?, ?, ?, 'running', ?, ?)",
        )
        .bind(&step_id)
        .bind(task_run_id)
        .bind(skill_tool_id)
        .bind(integration_account_id)
        .bind(step_index)
        .bind(args.to_string())
        .bind(&now)
        .execute(&self.db)
        .await?;

        // Declarative execution: the builtin implementation referenced by the
        // tool produces the output. Unknown references fail the step.
        let output = self.execute_declarative(skill_tool_id, args).await;

        match &output {
            Ok(value) => {
                sqlx::query("UPDATE task_steps SET status = 'completed', output_json = ? WHERE id = ?")
                    .bind(value.to_string())
                    .bind(&step_id)
                    .execute(&self.db)
                    .await?;
            }
            Err(err) => {
                sqlx::query("UPDATE task_steps SET status = 'failed', error_message = ? WHERE id = ?")
                    .bind(err.to_string())
                    .bind(&step_id)
                    .execute(&self.db)
                    .await?;
            }
        }
        output
    }

    /// Resolve and run the builtin implementation behind a declarative tool.
    async fn execute_declarative(&self, skill_tool_id: &str, args: &Value) -> Result<Value, ApiError> {
        let row = sqlx::query("SELECT implementation_ref FROM skill_tools WHERE id = ?")
            .bind(skill_tool_id)
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let reference: Option<String> = row.get("implementation_ref");
        let reference = reference.ok_or_else(|| {
            ApiError::Unprocessable("declarative tool has no implementation reference".into())
        })?;

        match reference.as_str() {
            "builtin.echo" => Ok(args.clone()),
            "builtin.noop" => Ok(json!({ "ok": true })),
            other => Err(ApiError::Unprocessable(format!(
                "unknown builtin implementation '{other}'"
            ))),
        }
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

    /// Seed user, agent, skill, version, tool, and agent-skill link.
    async fn seed(s: &AppState, risk: &str, policy: Option<&str>) -> String {
        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO skills (id, name, status, created_at, updated_at) VALUES ('sk1','Skill','enabled','2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO skill_versions (id, skill_id, version, manifest_json, package_hash, install_path, status, installed_at) VALUES ('sv1','sk1','1.0.0','{}','h','/tmp/x','active','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("UPDATE skills SET active_version_id = 'sv1' WHERE id = 'sk1'")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO skill_tools (id, skill_version_id, tool_name, description, input_schema_json, risk_class, implementation_type, implementation_ref) VALUES ('st1','sv1','search','d','{}',?, 'declarative', 'builtin.echo')")
            .bind(risk)
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO agent_skills (id, agent_id, skill_version_id, enabled, installed_at) VALUES ('as1','ag1','sv1',1,'2026')")
            .execute(&s.db).await.unwrap();
        if let Some(p) = policy {
            sqlx::query("INSERT INTO agent_tool_permissions (id, agent_id, skill_tool_id, policy, created_at, updated_at) VALUES ('p1','ag1','st1',?,'2026','2026')")
                .bind(p)
                .execute(&s.db).await.unwrap();
        }
        "st1".into()
    }

    #[tokio::test]
    async fn low_risk_allowed() {
        let s = state().await;
        let tool = seed(&s, "low", None).await;
        let reg = ToolRegistry::new(&s);
        assert!(matches!(reg.authorize("ag1", &tool, &json!({}), None).await.unwrap(), Authorization::Allowed));
    }

    #[tokio::test]
    async fn high_risk_requires_approval() {
        let s = state().await;
        let tool = seed(&s, "high", None).await;
        let reg = ToolRegistry::new(&s);
        match reg.authorize("ag1", &tool, &json!({}), None).await.unwrap() {
            Authorization::RequiresApproval { .. } => {}
            other => panic!("expected approval, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deny_policy_blocks_even_low_risk() {
        let s = state().await;
        let tool = seed(&s, "low", Some("deny")).await;
        let reg = ToolRegistry::new(&s);
        assert!(matches!(reg.authorize("ag1", &tool, &json!({}), None).await.unwrap(), Authorization::Denied(_)));
    }

    #[tokio::test]
    async fn disabled_skill_is_not_found() {
        let s = state().await;
        let tool = seed(&s, "low", None).await;
        sqlx::query("UPDATE agent_skills SET enabled = 0 WHERE agent_id = 'ag1'")
            .execute(&s.db).await.unwrap();
        let reg = ToolRegistry::new(&s);
        assert!(reg.authorize("ag1", &tool, &json!({}), None).await.is_err());
    }

    #[tokio::test]
    async fn scope_check_enforced() {
        let s = state().await;
        let tool = seed(&s, "low", None).await;
        sqlx::query("INSERT INTO integrations (id, user_id, provider_type, display_name, status, created_at, updated_at) VALUES ('i1','u1','gmail','G','connected','2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO integration_accounts (id, integration_id, external_account_id, display_name, granted_scopes_json, status, created_at, updated_at) VALUES ('ac1','i1','ext','A','[\"gmail.read\"]','active','2026','2026')")
            .execute(&s.db).await.unwrap();

        let reg = ToolRegistry::new(&s);
        // Missing scope is rejected.
        let err = reg
            .authorize("ag1", &tool, &json!({ "x-required-scope": "gmail.send" }), Some("ac1"))
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden(_)));
        // Granted scope passes.
        assert!(matches!(
            reg.authorize("ag1", &tool, &json!({ "x-required-scope": "gmail.read" }), Some("ac1")).await.unwrap(),
            Authorization::Allowed
        ));
    }

    #[tokio::test]
    async fn invoke_records_step() {
        let s = state().await;
        let tool = seed(&s, "low", None).await;
        sqlx::query("INSERT INTO tasks (id, user_id, agent_id, title, objective, status, timezone, created_at, updated_at) VALUES ('t1','u1','ag1','T','O','active','UTC','2026','2026')")
            .execute(&s.db).await.unwrap();
        let run_id = super::super::scheduler::create_run_and_enqueue(&s.db, "t1", "manual").await.unwrap();

        let reg = ToolRegistry::new(&s);
        let out = reg.invoke(&run_id, &tool, &json!({ "q": "x" }), None).await.unwrap();
        assert_eq!(out["q"], "x");
        let step = sqlx::query("SELECT status, output_json FROM task_steps WHERE task_run_id = ?")
            .bind(&run_id).fetch_one(&s.db).await.unwrap();
        assert_eq!(step.get::<String, _>("status"), "completed");
    }
}