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
    state: AppState,
    pub(crate) approvals: super::approvals::ApprovalService,
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
            state: state.clone(),
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
        self.authorize_in_conversation(agent_id, skill_tool_id, args, integration_account_id, None)
            .await
    }

    /// Authorize a tool invocation in a chat conversation. The conversation
    /// enables the per-chat auto-approve setting and binds any approval
    /// request to the chat so it can be decided inline.
    pub async fn authorize_in_conversation(
        &self,
        agent_id: &str,
        skill_tool_id: &str,
        args: &Value,
        integration_account_id: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<Authorization, ApiError> {
        // 1. Tool must exist and belong to a skill version enabled for the agent.
        let tool = sqlx::query(
            "SELECT st.tool_name, st.risk_class, st.implementation_type, st.implementation_ref, st.input_schema_json,
                    sv.manifest_json
             FROM skill_tools st
             JOIN skill_versions sv ON sv.id = st.skill_version_id
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

        // 1b. Skills that require an integration get the owner's active
        // account resolved automatically when the caller did not pass one:
        // the agent has no way to know account ids, so requiring it in the
        // arguments would make every integration tool unusable in chat.
        let integration_account_id = match integration_account_id {
            Some(id) => Some(id.to_string()),
            None => {
                let manifest: String = tool.get("manifest_json");
                let providers = serde_json::from_str::<serde_json::Value>(&manifest)
                    .ok()
                    .and_then(|m| {
                        m.get("required_integrations")
                            .and_then(|v| v.as_array().cloned())
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>();
                match providers.first() {
                    Some(provider) => {
                        let account: Option<String> = sqlx::query(
                            "SELECT a.id FROM integration_accounts a
                             JOIN integrations i ON i.id = a.integration_id
                             JOIN agents ag ON ag.user_id = i.user_id
                             WHERE ag.id = ? AND i.provider_type = ? AND i.status = 'connected' AND a.status = 'active'
                             ORDER BY a.created_at LIMIT 1",
                        )
                        .bind(agent_id)
                        .bind(provider)
                        .fetch_optional(&self.db)
                        .await?
                        .and_then(|r| r.try_get("id").ok());
                        match account {
                            Some(id) => Some(id),
                            None => {
                                return Ok(Authorization::Denied(format!(
                                    "this tool needs a connected {provider} account; connect it in the Integrations view"
                                )));
                            }
                        }
                    }
                    None => None,
                }
            }
        };
        let integration_account_id = integration_account_id.as_deref();

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
            // Auto-approve settings: a per-conversation override wins over
            // the global setting. "critical" risk always asks regardless.
            let auto_approved = risk_class != "critical"
                && self.auto_approve(conversation_id).await;
            if auto_approved {
                return Ok(Authorization::Allowed);
            }
            let operation = json!({
                "agent_id": agent_id,
                "skill_tool_id": skill_tool_id,
                "tool_name": tool_name,
                "args": args,
                "integration_account_id": integration_account_id,
            });
            // A prior approval for this exact operation (same hash) lets the
            // call through: the user already decided it, and the runtime
            // retries the tool after the approval instead of blocking the
            // chat exchange for hours.
            let approved = sqlx::query(
                "SELECT id FROM approval_requests
                 WHERE skill_tool_id = ? AND operation_hash = ? AND status = 'approved' AND expires_at > ?
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(skill_tool_id)
            .bind(crate::services::approvals::operation_hash(&operation))
            .bind(chrono::Utc::now().to_rfc3339())
            .fetch_optional(&self.db)
            .await?;
            if approved.is_some() {
                return Ok(Authorization::Allowed);
            }
            let approval_id = self
                .approvals
                .create_with_conversation(None, Some(skill_tool_id), conversation_id, &operation)
                .await
                .map_err(ApiError::from)?;
            return Ok(Authorization::RequiresApproval { approval_id });
        }
        Ok(Authorization::Allowed)
    }

    /// Whether tool executions in this context run without approval: the
    /// per-conversation setting wins; otherwise the global setting applies.
    async fn auto_approve(&self, conversation_id: Option<&str>) -> bool {
        if let Some(conversation_id) = conversation_id {
            let row = sqlx::query(
                "SELECT value_json FROM app_settings WHERE key = ?",
            )
            .bind(format!("auto_approve:conversation:{conversation_id}"))
            .fetch_optional(&self.db)
            .await
            .ok()
            .flatten();
            if let Some(row) = row {
                let raw: String = row.get("value_json");
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
                        return enabled;
                    }
                }
            }
        }
        let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = 'auto_approve'")
            .fetch_optional(&self.db)
            .await
            .ok()
            .flatten();
        match row {
            Some(row) => {
                let raw: String = row.get("value_json");
                serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|v| v.get("enabled").and_then(|v| v.as_bool()))
                    .unwrap_or(false)
            }
            None => false,
        }
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

    /// Invoke a tool during a chat conversation. No task run exists, so the
    /// call is audited but not recorded as a task step.
    pub async fn invoke_chat(
        &self,
        agent_id: &str,
        skill_tool_id: &str,
        args: &Value,
    ) -> Result<Value, ApiError> {
        let account_id = match args.get("integration_account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => Some(id.to_string()),
            _ => self.resolve_skill_account(agent_id, skill_tool_id).await?,
        };
        let args = if account_id.is_some() && args.get("integration_account_id").is_none() {
            let mut args = args.clone();
            args["integration_account_id"] = json!(account_id);
            args
        } else {
            args.clone()
        };
        self.execute_declarative_with_account(skill_tool_id, &args, account_id.as_deref()).await
    }

    /// Resolve the skill owner's active account for the skill's required
    /// integration (first provider in `required_integrations`).
    async fn resolve_skill_account(
        &self,
        agent_id: &str,
        skill_tool_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let row = sqlx::query(
            "SELECT sv.manifest_json FROM skill_tools st
             JOIN skill_versions sv ON sv.id = st.skill_version_id
             WHERE st.id = ?",
        )
        .bind(skill_tool_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;
        let manifest: String = row.get("manifest_json");
        let provider = serde_json::from_str::<serde_json::Value>(&manifest)
            .ok()
            .and_then(|m| {
                m.get("required_integrations")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
            });
        let Some(provider) = provider else { return Ok(None) };
        let account: Option<String> = sqlx::query(
            "SELECT a.id FROM integration_accounts a
             JOIN integrations i ON i.id = a.integration_id
             JOIN agents ag ON ag.user_id = i.user_id
             WHERE ag.id = ? AND i.provider_type = ? AND i.status = 'connected' AND a.status = 'active'
             ORDER BY a.created_at LIMIT 1",
        )
        .bind(agent_id)
        .bind(&provider)
        .fetch_optional(&self.db)
        .await?
        .and_then(|r| r.try_get("id").ok());
        Ok(account)
    }

    /// Resolve and run the builtin implementation behind a declarative tool.
    async fn execute_declarative(&self, skill_tool_id: &str, args: &Value) -> Result<Value, ApiError> {
        let account_id = args.get("integration_account_id").and_then(|v| v.as_str());
        self.execute_declarative_with_account(skill_tool_id, args, account_id).await
    }

    /// Resolve and run the implementation, with an explicit account override
    /// (used by chat invocations where the account comes from the arguments).
    async fn execute_declarative_with_account(
        &self,
        skill_tool_id: &str,
        args: &Value,
        account_id: Option<&str>,
    ) -> Result<Value, ApiError> {
        let row = sqlx::query(
            "SELECT st.implementation_ref, sv.skill_id, sk.id AS skill_row_id
             FROM skill_tools st
             JOIN skill_versions sv ON sv.id = st.skill_version_id
             JOIN skills sk ON sk.id = sv.skill_id
             WHERE st.id = ?",
        )
        .bind(skill_tool_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;
        let reference: Option<String> = row.get("implementation_ref");
        let reference = reference.ok_or_else(|| {
            ApiError::Unprocessable("declarative tool has no implementation reference".into())
        })?;

        // Task-management and subagent tools operate on the user's tasks and
        // agents through the TaskService; the acting user is resolved from
        // the agent's owner. Checked before builtin normalization: these
        // refs are bare names.
        if reference.starts_with("task_") || reference.starts_with("subagent_") {
            let owner: String = sqlx::query(
                "SELECT a.user_id FROM agents a
                 JOIN agent_skills ask ON ask.agent_id = a.id
                 JOIN skill_versions sv ON sv.id = ask.skill_version_id
                 JOIN skills sk ON sk.id = sv.skill_id
                 WHERE sk.id = ? LIMIT 1",
            )
            .bind(row.get::<String, _>("skill_row_id"))
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?
            .try_get("user_id")?;
            return super::task_tools::execute_task_tool(&self.state, &reference, args, &owner).await;
        }

        // Integration tools dispatch to the integration modules; they need
        // the connected account passed in the arguments. Tools whose
        // implementation is a registered builtin (github_*, git_*) are NOT
        // integration dispatches: the builtin itself consumes the account
        // from the arguments, so they fall through to the builtin path.
        if let Some(account_id) = account_id {
            let is_builtin_ref = reference.starts_with("builtin.")
                || super::skill_installer::KNOWN_BUILTINS
                    .iter()
                    .any(|k| k.strip_prefix("builtin.") == Some(reference.as_str()));
            if !is_builtin_ref {
                let provider_type: String = sqlx::query("SELECT provider_type FROM integrations WHERE id = (SELECT integration_id FROM integration_accounts WHERE id = ?)")
                    .bind(account_id)
                    .fetch_optional(&self.db)
                    .await?
                    .ok_or(ApiError::NotFound)?
                    .try_get("provider_type")?;
                return super::integrations::execute_integration_tool(
                    &self.state,
                    &provider_type,
                    &reference,
                    args,
                    account_id,
                )
                .await;
            }
        }

        // Normalize the bare form ("code_write_file") to the namespaced
        // builtin form ("builtin.code_write_file") for the builtin dispatch.
        let reference = if reference.starts_with("builtin.") {
            reference
        } else {
            format!("builtin.{reference}")
        };

        super::builtins::execute_builtin(&self.state, &reference, args).await
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
    async fn global_auto_approve_skips_approval() {
        let s = state().await;
        let tool = seed(&s, "high", None).await;
        sqlx::query("INSERT INTO app_settings (key, value_json, updated_at) VALUES ('auto_approve', '{\"enabled\":true}', '2026')")
            .execute(&s.db).await.unwrap();
        let reg = ToolRegistry::new(&s);
        assert!(matches!(reg.authorize("ag1", &tool, &json!({}), None).await.unwrap(), Authorization::Allowed));
    }

    #[tokio::test]
    async fn critical_risk_always_asks_even_with_auto_approve() {
        let s = state().await;
        let tool = seed(&s, "critical", None).await;
        sqlx::query("INSERT INTO conversations (id, user_id, agent_id, title, status, created_at, updated_at) VALUES ('c1','u1','ag1','T','active','2026','2026')")
            .execute(&s.db).await.unwrap();
        sqlx::query("INSERT INTO app_settings (key, value_json, updated_at) VALUES ('auto_approve', '{\"enabled\":true}', '2026')")
            .execute(&s.db).await.unwrap();
        let reg = ToolRegistry::new(&s);
        assert!(matches!(
            reg.authorize_in_conversation("ag1", &tool, &json!({}), None, Some("c1")).await.unwrap(),
            Authorization::RequiresApproval { .. }
        ));
    }

    #[tokio::test]
    async fn approval_created_in_conversation_is_bound_to_it() {
        let s = state().await;
        let tool = seed(&s, "high", None).await;
        sqlx::query("INSERT INTO conversations (id, user_id, agent_id, title, status, created_at, updated_at) VALUES ('c1','u1','ag1','T','active','2026','2026')")
            .execute(&s.db).await.unwrap();
        let reg = ToolRegistry::new(&s);
        let approval_id = match reg.authorize_in_conversation("ag1", &tool, &json!({}), None, Some("c1")).await.unwrap() {
            Authorization::RequiresApproval { approval_id } => approval_id,
            other => panic!("expected approval, got {other:?}"),
        };
        let conversation_id: Option<String> =
            sqlx::query("SELECT conversation_id FROM approval_requests WHERE id = ?")
                .bind(&approval_id)
                .fetch_one(&s.db)
                .await
                .unwrap()
                .try_get("conversation_id")
                .unwrap();
        assert_eq!(conversation_id.as_deref(), Some("c1"));
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