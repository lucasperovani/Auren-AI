//! Background workers: scheduler tick and queue worker (spec section 2.2).

use std::time::Duration;

use sqlx::Row;

use crate::{services::scheduler, state::AppState};

/// How often the scheduler and queue worker run.
const TICK_INTERVAL: Duration = Duration::from_secs(10);
const LEASE_SECS: i64 = 120;
const MAX_ATTEMPTS: i64 = 5;

/// Run the scheduler and queue worker loop until the process exits.
pub async fn run(state: AppState) {
    let worker_id = uuid::Uuid::new_v4().to_string();
    let mut interval = tokio::time::interval(TICK_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(e) = tick_once(&state, &worker_id).await {
            tracing::error!(error = %e, "scheduler tick failed");
        }
    }
}

pub async fn tick_once(state: &AppState, worker_id: &str) -> anyhow::Result<()> {
    // Reclaim leases abandoned by crashed workers.
    let reclaimed = scheduler::reclaim_expired_leases(&state.db).await?;
    if reclaimed > 0 {
        tracing::debug!(reclaimed, "reclaimed expired job leases");
    }

    // Advance due schedules into the queue.
    let fired = scheduler::tick(&state.db).await?;
    if fired > 0 {
        tracing::debug!(fired, "scheduled runs enqueued");
    }

    // Claim and execute due jobs.
    let jobs = scheduler::claim_jobs(&state.db, worker_id, LEASE_SECS, 10).await?;
    for job_id in jobs {
        let run_row = sqlx::query("SELECT task_run_id FROM queued_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(&state.db)
            .await?;
        let run_id: String = run_row.get("task_run_id");

        // Mark the run running and execute it.
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE task_runs SET status = 'running', started_at = ? WHERE id = ?")
            .bind(&now)
            .bind(&run_id)
            .execute(&state.db)
            .await?;

        // Execute through the bounded agent runtime: it plans with the LLM
        // when a provider is configured, and falls back to recorded steps
        // otherwise.
        let runtime = crate::services::agent_runtime::AgentRuntime::new(state.clone());
        match runtime.execute(&run_id).await {
            crate::services::agent_runtime::RunOutcome::Completed => {
                let result_json: String = sqlx::query("SELECT result_json FROM task_runs WHERE id = ?")
                    .bind(&run_id)
                    .fetch_one(&state.db)
                    .await?
                    .try_get("result_json")
                    .unwrap_or_else(|_| "{}".to_string());
                scheduler::complete_job(&state.db, &job_id, &run_id, &result_json).await?;
            }
            crate::services::agent_runtime::RunOutcome::Paused => {
                // Waiting for approval or explicit pause: release the lease
                // without completing.
                sqlx::query("UPDATE queued_jobs SET status = 'done', lease_owner = NULL WHERE id = ?")
                    .bind(&job_id)
                    .execute(&state.db)
                    .await?;
            }
            crate::services::agent_runtime::RunOutcome::Failed(reason) => {
                scheduler::fail_job(&state.db, &job_id, &run_id, &reason, MAX_ATTEMPTS).await?;
            }
            crate::services::agent_runtime::RunOutcome::LimitReached(reason) => {
                scheduler::fail_job(&state.db, &job_id, &run_id, &reason, MAX_ATTEMPTS).await?;
            }
        }
    }

    // Retry operations that were deferred by a provider rate limit. They run
    // exactly once per scheduled attempt: no polling, no agent involvement.
    retry_deferred_operations(state).await?;

    Ok(())
}

/// How many deferred operations to attempt per tick.
const DEFERRED_BATCH: i64 = 5;

/// Run deferred provider operations whose rate-limit window has reopened.
async fn retry_deferred_operations(state: &AppState) -> anyhow::Result<()> {
    let operations = crate::services::rate_limits::due_operations(state, DEFERRED_BATCH)
        .await
        .map_err(|e| anyhow::anyhow!("failed to load deferred operations: {e}"))?;

    for operation in operations {
        let result = crate::services::integrations::execute_integration_tool(
            state,
            // The provider is resolved from the account, never trusted from
            // the stored payload.
            &provider_of(state, &operation.account_id).await?,
            &operation.operation,
            &operation.payload,
            &operation.account_id,
        )
        .await;

        match result {
            Ok(value) => {
                // Record the usage now that the call went through.
                if let Some(policy) =
                    crate::services::rate_limits::policy_for(
                        &provider_of(state, &operation.account_id).await?,
                        &operation.operation,
                    )
                {
                    let _ = crate::services::rate_limits::consume(
                        state,
                        &operation.account_id,
                        policy,
                    )
                    .await;
                }
                crate::services::rate_limits::complete(state, &operation, &value).await;
            }
            Err(crate::error::ApiError::TooManyRequests) => {
                // Still limited: wait for the next window instead of burning
                // attempts.
                let policy = crate::services::rate_limits::policy_for(
                    &provider_of(state, &operation.account_id).await?,
                    &operation.operation,
                );
                let retry_at = match policy {
                    Some(p) => crate::services::rate_limits::status(state, &operation.account_id, p)
                        .await
                        .map(|q| q.resets_at.to_rfc3339())
                        .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339()),
                    None => chrono::Utc::now().to_rfc3339(),
                };
                let _ = sqlx::query(
                    "UPDATE deferred_operations SET available_at = ?, updated_at = ? WHERE id = ?",
                )
                .bind(&retry_at)
                .bind(chrono::Utc::now().to_rfc3339())
                .bind(&operation.id)
                .execute(&state.db)
                .await;
            }
            Err(error) => {
                crate::services::rate_limits::retry_later(state, &operation, &error.to_string())
                    .await;
            }
        }
    }
    Ok(())
}

/// Provider type behind an integration account.
async fn provider_of(state: &AppState, account_id: &str) -> anyhow::Result<String> {
    let row = sqlx::query(
        "SELECT i.provider_type FROM integrations i
         JOIN integration_accounts a ON a.integration_id = i.id
         WHERE a.id = ?",
    )
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("integration account {account_id} no longer exists"))?;
    Ok(row.try_get("provider_type")?)
}
