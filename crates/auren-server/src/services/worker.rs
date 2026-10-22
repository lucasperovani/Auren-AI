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
    Ok(())
}
