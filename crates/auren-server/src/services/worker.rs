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

        // Execute the run: run each pending step through the permission
        // engine. The declarative runtime composes registered builtins.
        let result = execute_run(state, &run_id).await;
        match result {
            Ok(output) => {
                scheduler::complete_job(&state.db, &job_id, &run_id, &output).await?;
            }
            Err(e) => {
                scheduler::fail_job(&state.db, &job_id, &run_id, &e.to_string(), MAX_ATTEMPTS).await?;
            }
        }
    }
    Ok(())
}

/// Execute a task run: iterate its steps (or synthesize a single echo step
/// from the task objective when no plan exists) through the ToolRegistry.
async fn execute_run(state: &AppState, run_id: &str) -> anyhow::Result<String> {
    let task_row = sqlx::query("SELECT task_id FROM task_runs WHERE id = ?")
        .bind(run_id)
        .fetch_one(&state.db)
        .await?;
    let task_id: String = task_row.get("task_id");

    let steps = sqlx::query("SELECT * FROM task_steps WHERE task_run_id = ? ORDER BY step_index")
        .bind(run_id)
        .fetch_all(&state.db)
        .await?;

    let registry = crate::services::tool_registry::ToolRegistry::new(state);
    let mut outputs = Vec::new();

    if steps.is_empty() {
        // No planned steps: record the objective as a single declarative step.
        let task = sqlx::query("SELECT objective FROM tasks WHERE id = ?")
            .bind(&task_id)
            .fetch_one(&state.db)
            .await?;
        let objective: String = task.get("objective");
        let output = serde_json::json!({ "objective": objective, "status": "acknowledged" });
        outputs.push(output.clone());
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO task_steps (id, task_run_id, step_index, status, input_json, output_json, created_at)
             VALUES (?, ?, 0, 'completed', ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(run_id)
        .bind(serde_json::json!({ "objective": objective }).to_string())
        .bind(output.to_string())
        .bind(&now)
        .execute(&state.db)
        .await?;
    } else {
        for step in steps {
            let step_id: String = step.get("id");
            let status: String = step.get("status");
            if status == "completed" || status == "skipped" {
                continue;
            }
            let skill_tool_id: Option<String> = step.get("skill_tool_id");
            let input_json: String = step.get("input_json");
            let input: serde_json::Value = serde_json::from_str(&input_json).unwrap_or(serde_json::json!({}));
            let account_id: Option<String> = step.get("integration_account_id");

            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query("UPDATE task_steps SET status = 'running' WHERE id = ?")
                .bind(&step_id)
                .bind(&now)
                .execute(&state.db)
                .await?;

            match registry.invoke(run_id, skill_tool_id.as_deref().unwrap_or_default(), &input, account_id.as_deref()).await {
                Ok(output) => outputs.push(output),
                Err(e) => return Err(anyhow::anyhow!("step failed: {e}")),
            }
        }
    }

    Ok(serde_json::json!({ "outputs": outputs }).to_string())
}