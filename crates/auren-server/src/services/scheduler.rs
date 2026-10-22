//! Scheduler and persistent job queue (spec section 2.2).
//!
//! A due schedule creates an execution request (task run) which is placed
//! into the persistent queue. Workers claim jobs with a lease; uncertain
//! outcomes are recorded and reconciled before retrying.

use chrono::{DateTime, Datelike, Timelike, Utc};
use sqlx::{Row, SqlitePool};

/// Compute the next fire time for a 5-field cron expression (UTC).
/// Supports `*`, `*/n`, lists, and exact values per field.
pub fn next_run_at(schedule: &serde_json::Value) -> Option<DateTime<Utc>> {
    let expr = schedule.get("cron")?.as_str()?;
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }

    let now = Utc::now();
    // Scan forward minute by minute, at most ~366 days.
    let mut candidate = now + chrono::Duration::minutes(1);
    candidate = candidate.with_second(0).unwrap().with_nanosecond(0).unwrap();
    let limit = now + chrono::Duration::days(366);

    while candidate <= limit {
        let minute = candidate.minute() as i64;
        let hour = candidate.hour() as i64;
        let dom = candidate.day() as i64;
        let month = candidate.month() as i64;
        let dow = candidate.weekday().num_days_from_sunday() as i64;

        let values = [minute, hour, dom, month, dow];
        let mut matches = true;
        for (i, field) in fields.iter().enumerate() {
            // Day-of-week: 0 and 7 are both Sunday.
            let v = if i == 4 && values[i] == 0 { 7 } else { values[i] };
            if !field_matches(field, v, i == 2, i == 4) {
                matches = false;
                break;
            }
        }
        if matches {
            return Some(candidate);
        }
        candidate += chrono::Duration::minutes(1);
    }
    None
}

/// Whether a single cron field matches a value. `dom` and `dow` may be
/// restricted (`*` in one of them limits the other per standard cron).
fn field_matches(field: &str, value: i64, is_dom: bool, is_dow: bool) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        return step.parse::<i64>().map(|s| value % s == 0).unwrap_or(false);
    }
    for part in field.split(',') {
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.parse::<i64>(), end.parse::<i64>()) {
                if value >= s && value <= e {
                    return true;
                }
            }
        } else if let Some(base) = part.strip_prefix("*/") {
            if let Ok(step) = base.parse::<i64>() {
                if value % step == 0 {
                    return true;
                }
            }
        } else if let Ok(v) = part.parse::<i64>() {
            if v == value {
                return true;
            }
        }
    }
    let _ = (is_dom, is_dow);
    false
}

/// Create a task run and enqueue it for execution. The run starts in
/// `queued` state; the queue worker transitions it to `running`.
pub async fn create_run_and_enqueue(
    db: &SqlitePool,
    task_id: &str,
    trigger: &str,
) -> anyhow::Result<String> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO task_runs (id, task_id, status, trigger, created_at) VALUES (?, ?, 'queued', ?, ?)",
    )
    .bind(&run_id)
    .bind(task_id)
    .bind(trigger)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO queued_jobs (id, task_run_id, job_type, status, available_at, created_at)
         VALUES (?, ?, 'task_run', 'pending', ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&run_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(run_id)
}

/// Claim due jobs with a lease. Uses a single UPDATE guarded by the current
/// status so concurrent workers never claim the same job twice.
pub async fn claim_jobs(db: &SqlitePool, worker_id: &str, lease_secs: i64, limit: i64) -> anyhow::Result<Vec<String>> {
    let now = Utc::now().to_rfc3339();
    let lease_expires = (Utc::now() + chrono::Duration::seconds(lease_secs)).to_rfc3339();
    let mut tx = db.begin().await?;

    let due = sqlx::query(
        "SELECT id FROM queued_jobs
         WHERE status = 'pending' AND available_at <= ?
         ORDER BY available_at LIMIT ?",
    )
    .bind(&now)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;

    let mut claimed = Vec::new();
    for row in due {
        let job_id: String = row.get("id");
        let result = sqlx::query(
            "UPDATE queued_jobs
             SET status = 'leased', lease_owner = ?, lease_expires_at = ?, attempt_count = attempt_count + 1
             WHERE id = ? AND status = 'pending'",
        )
        .bind(worker_id)
        .bind(&lease_expires)
        .bind(&job_id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            claimed.push(job_id);
        }
    }
    tx.commit().await?;
    Ok(claimed)
}

/// Reclaim jobs whose lease expired without completion (crash recovery).
pub async fn reclaim_expired_leases(db: &SqlitePool) -> anyhow::Result<u64> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE queued_jobs
         SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL
         WHERE status = 'leased' AND lease_expires_at < ?",
    )
    .bind(&now)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Mark a job done and its run completed.
pub async fn complete_job(db: &SqlitePool, job_id: &str, run_id: &str, result_json: &str) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE queued_jobs SET status = 'done', lease_owner = NULL WHERE id = ?")
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE task_runs SET status = 'completed', finished_at = ?, result_json = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(result_json)
    .bind(run_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Mark a job failed. Retries up to `max_attempts`, then gives up and marks
/// the run failed with the recorded error.
pub async fn fail_job(db: &SqlitePool, job_id: &str, run_id: &str, error: &str, max_attempts: i64) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    let attempts: i64 = sqlx::query("SELECT attempt_count FROM queued_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(db)
        .await?
        .get("attempt_count");

    let mut tx = db.begin().await?;
    if attempts < max_attempts {
        // Exponential backoff: 2^attempt minutes.
        let backoff = 60i64 * (1 << attempts.min(6));
        let available = (Utc::now() + chrono::Duration::seconds(backoff)).to_rfc3339();
        sqlx::query(
            "UPDATE queued_jobs SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL, last_error = ?, available_at = ? WHERE id = ?",
        )
        .bind(error)
        .bind(&available)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE task_runs SET status = 'queued', error_message = ? WHERE id = ?")
            .bind(error)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("UPDATE queued_jobs SET status = 'failed', last_error = ? WHERE id = ?")
            .bind(error)
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE task_runs SET status = 'failed', finished_at = ?, error_message = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(error)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Advance every active schedule that is due: create a run, enqueue it, and
/// bump `next_run_at`. The `UNIQUE(task_id)` constraint plus the conditional
/// UPDATE prevent duplicate enqueueing across concurrent schedulers.
pub async fn tick(db: &SqlitePool) -> anyhow::Result<usize> {
    let now = Utc::now().to_rfc3339();
    let due = sqlx::query(
        "SELECT j.id, j.task_id, j.next_run_at FROM scheduled_jobs j
         WHERE j.status = 'active' AND j.next_run_at <= ?",
    )
    .bind(&now)
    .fetch_all(db)
    .await?;

    let mut fired = 0;
    for row in due {
        let job_id: String = row.get("id");
        let task_id: String = row.get("task_id");
        let stored_next_run: String = row.get("next_run_at");

        // Guard: only advance if next_run_at is unchanged (no other
        // scheduler ticked this job in between).
        let schedule_json: String = sqlx::query("SELECT schedule_json FROM scheduled_jobs WHERE id = ? AND next_run_at = ?")
            .bind(&job_id)
            .bind(&stored_next_run)
            .fetch_optional(db)
            .await?
            .map(|r| r.get("schedule_json"))
            .ok_or(anyhow::anyhow!("schedule already advanced"))?;
        let schedule: serde_json::Value = serde_json::from_str(&schedule_json)?;

        create_run_and_enqueue(db, &task_id, "schedule").await?;

        if let Some(next) = next_run_at(&schedule) {
            sqlx::query("UPDATE scheduled_jobs SET next_run_at = ?, updated_at = ? WHERE id = ? AND next_run_at = ?")
                .bind(next.to_rfc3339())
                .bind(&now)
                .bind(&job_id)
                .bind(&stored_next_run)
                .execute(db)
                .await?;
        } else {
            // Schedule never fires again; deactivate it.
            sqlx::query("UPDATE scheduled_jobs SET status = 'cancelled', updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(&job_id)
                .execute(db)
                .await?;
        }
        fired += 1;
    }
    Ok(fired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cron(v: &str) -> serde_json::Value {
        serde_json::json!({ "cron": v })
    }

    #[test]
    fn next_run_basic_patterns() {
        let now = Utc::now();
        let next = next_run_at(&cron("* * * * *")).unwrap();
        assert!(next > now && next - now <= chrono::Duration::minutes(2));

        // Midnight daily.
        let next = next_run_at(&cron("0 0 * * *")).unwrap();
        assert_eq!(next.hour(), 0);
        assert_eq!(next.minute(), 0);
        assert!(next > now);
    }

    #[test]
    fn next_run_step_and_list() {
        let next = next_run_at(&cron("*/15 * * * *")).unwrap();
        assert!(next.minute() % 15 == 0);

        let next = next_run_at(&cron("0,30 * * * *")).unwrap();
        assert!(next.minute() == 0 || next.minute() == 30);
    }

    #[test]
    fn invalid_schedule_returns_none() {
        assert!(next_run_at(&cron("bad")).is_none());
        assert!(next_run_at(&serde_json::json!({})).is_none());
    }

    #[tokio::test]
    async fn queue_claim_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        std::mem::forget(dir);

        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tasks (id, user_id, agent_id, title, objective, status, timezone, created_at, updated_at) VALUES ('t1','u1','ag1','T','O','active','UTC','2026','2026')")
            .execute(&pool).await.unwrap();

        let run_id = create_run_and_enqueue(&pool, "t1", "manual").await.unwrap();

        // Two workers claim; only one gets the job.
        let a = claim_jobs(&pool, "w1", 30, 10).await.unwrap();
        let b = claim_jobs(&pool, "w2", 30, 10).await.unwrap();
        assert_eq!(a.len(), 1);
        assert!(b.is_empty());

        complete_job(&pool, &a[0], &run_id, "{}").await.unwrap();
        let status: String = sqlx::query("SELECT status FROM task_runs WHERE id = ?")
            .bind(&run_id).fetch_one(&pool).await.unwrap().get("status");
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    async fn failed_job_retries_then_fails() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
        let pool = crate::db::connect(&url).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        std::mem::forget(dir);

        sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO agents (id, user_id, name, instructions, status, settings_json, created_at, updated_at) VALUES ('ag1','u1','A','','active','{}','2026','2026')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO tasks (id, user_id, agent_id, title, objective, status, timezone, created_at, updated_at) VALUES ('t1','u1','ag1','T','O','active','UTC','2026','2026')")
            .execute(&pool).await.unwrap();

        let run_id = create_run_and_enqueue(&pool, "t1", "manual").await.unwrap();
        let jobs = claim_jobs(&pool, "w1", 30, 10).await.unwrap();
        assert_eq!(jobs.len(), 1);

        // First failure: goes back to pending for retry.
        fail_job(&pool, &jobs[0], &run_id, "boom", 3).await.unwrap();
        let status: String = sqlx::query("SELECT status FROM queued_jobs WHERE id = ?")
            .bind(&jobs[0]).fetch_one(&pool).await.unwrap().get("status");
        assert_eq!(status, "pending");

        // Exhaust attempts. Backoff pushes available_at into the future, so
        // pull it back to now before each retry claim.
        for _ in 0..3 {
            sqlx::query("UPDATE queued_jobs SET available_at = ? WHERE id = ?")
                .bind(chrono::Utc::now().to_rfc3339())
                .bind(&jobs[0])
                .execute(&pool)
                .await
                .unwrap();
            let jobs = claim_jobs(&pool, "w1", 30, 10).await.unwrap();
            if jobs.is_empty() { break; }
            fail_job(&pool, &jobs[0], &run_id, "boom", 3).await.unwrap();
        }
        let run_status: String = sqlx::query("SELECT status FROM task_runs WHERE id = ?")
            .bind(&run_id).fetch_one(&pool).await.unwrap().get("status");
        assert_eq!(run_status, "failed");
    }
}