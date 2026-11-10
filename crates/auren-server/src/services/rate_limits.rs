//! Provider rate-limit accounting and deferred execution.
//!
//! Providers publish hard quotas (Instagram, for example, allows a limited
//! number of API-published posts per rolling day). When a quota is exhausted
//! the operation is not failed and never polled: it is persisted in
//! `deferred_operations` with the moment the window reopens, the user is
//! notified, and the background worker picks it up once.
//!
//! Retries use capped exponential backoff so a provider outage cannot turn
//! into a hot loop, and no LLM call is involved in waiting.

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::Row;

use crate::{error::ApiError, state::AppState};

/// A quota definition for one logical operation bucket.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitPolicy {
    /// Logical bucket name, e.g. `publish` or `messages`.
    pub bucket: &'static str,
    /// Rolling window length in seconds.
    pub window_seconds: i64,
    /// Maximum number of operations allowed in the window.
    pub limit: i64,
}

/// Instagram content publishing: the `media_publish` endpoint accepts 50
/// posts per 24-hour moving period per professional account. Carousels and
/// Reels count as one post.
pub const INSTAGRAM_PUBLISH: RateLimitPolicy = RateLimitPolicy {
    bucket: "publish",
    window_seconds: 86_400,
    limit: 50,
};

/// Conservative outbound messaging quota. The provider allows far higher
/// burst rates, but a personal assistant should never saturate an account;
/// this cap keeps automated replies well under platform limits.
pub const INSTAGRAM_MESSAGES: RateLimitPolicy = RateLimitPolicy {
    bucket: "messages",
    window_seconds: 3_600,
    limit: 200,
};

/// Generic read traffic: a soft ceiling that trips long before the provider
/// rate limits the app, so reads degrade gracefully instead of failing.
pub const INSTAGRAM_READS: RateLimitPolicy = RateLimitPolicy {
    bucket: "reads",
    window_seconds: 3_600,
    limit: 400,
};

/// Result of a quota check.
#[derive(Debug, Clone)]
pub struct QuotaStatus {
    pub bucket: String,
    pub limit: i64,
    pub used: i64,
    /// When the current window rolls over and capacity becomes available.
    pub resets_at: DateTime<Utc>,
}

impl QuotaStatus {
    pub fn remaining(&self) -> i64 {
        (self.limit - self.used).max(0)
    }

    pub fn exhausted(&self) -> bool {
        self.used >= self.limit
    }
}

/// Resolve the policy for a provider operation, if it is metered.
pub fn policy_for(provider_type: &str, operation: &str) -> Option<RateLimitPolicy> {
    match (provider_type, operation) {
        ("instagram", "publish_media") => Some(INSTAGRAM_PUBLISH),
        ("instagram", "send_message") | ("instagram", "reply_to_comment") => Some(INSTAGRAM_MESSAGES),
        ("instagram", _) => Some(INSTAGRAM_READS),
        _ => None,
    }
}

/// Current window status for a bucket. A window that has already rolled over
/// reports zero usage against a fresh window.
pub async fn status(
    state: &AppState,
    account_id: &str,
    policy: RateLimitPolicy,
) -> Result<QuotaStatus, ApiError> {
    let row = sqlx::query(
        "SELECT used, limit_value, window_started_at FROM rate_limit_usage
         WHERE integration_account_id = ? AND bucket = ?",
    )
    .bind(account_id)
    .bind(policy.bucket)
    .fetch_optional(&state.db)
    .await?;

    let now = Utc::now();
    let Some(row) = row else {
        return Ok(QuotaStatus {
            bucket: policy.bucket.to_string(),
            limit: policy.limit,
            used: 0,
            resets_at: now + Duration::seconds(policy.window_seconds),
        });
    };

    let used: i64 = row.get("used");
    let window_started: String = row.get("window_started_at");
    let started = DateTime::parse_from_rfc3339(&window_started)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now);
    let resets_at = started + Duration::seconds(policy.window_seconds);

    if now >= resets_at {
        // The window already rolled over: capacity is fully restored.
        return Ok(QuotaStatus {
            bucket: policy.bucket.to_string(),
            limit: policy.limit,
            used: 0,
            resets_at: now + Duration::seconds(policy.window_seconds),
        });
    }

    Ok(QuotaStatus {
        bucket: policy.bucket.to_string(),
        limit: policy.limit,
        used,
        resets_at,
    })
}

/// Consume one unit of the quota. Rolls the window over when it has expired,
/// so accounting never carries usage across windows.
pub async fn consume(
    state: &AppState,
    account_id: &str,
    policy: RateLimitPolicy,
) -> Result<QuotaStatus, ApiError> {
    let current = status(state, account_id, policy).await?;
    let now = Utc::now();

    // A fresh window starts either on the first use or on rollover.
    let window_started = if current.used == 0 {
        now
    } else {
        current.resets_at - Duration::seconds(policy.window_seconds)
    };
    let used = current.used + 1;

    sqlx::query(
        "INSERT INTO rate_limit_usage
            (id, integration_account_id, bucket, window_seconds, used, limit_value, window_started_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(integration_account_id, bucket) DO UPDATE SET
            used = excluded.used,
            limit_value = excluded.limit_value,
            window_seconds = excluded.window_seconds,
            window_started_at = excluded.window_started_at,
            updated_at = excluded.updated_at",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(account_id)
    .bind(policy.bucket)
    .bind(policy.window_seconds)
    .bind(used)
    .bind(policy.limit)
    .bind(window_started.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&state.db)
    .await?;

    Ok(QuotaStatus {
        bucket: policy.bucket.to_string(),
        limit: policy.limit,
        used,
        resets_at: window_started + Duration::seconds(policy.window_seconds),
    })
}

/// Snapshot of every metered bucket for an account, for the settings view.
pub async fn account_usage(
    state: &AppState,
    account_id: &str,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT bucket, used, limit_value, window_seconds, window_started_at
         FROM rate_limit_usage WHERE integration_account_id = ? ORDER BY bucket",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await?;

    let now = Utc::now();
    let mut items = Vec::new();
    for row in rows {
        let bucket: String = row.get("bucket");
        let used: i64 = row.get("used");
        let limit: i64 = row.get("limit_value");
        let window_seconds: i64 = row.get("window_seconds");
        let started: String = row.get("window_started_at");
        let started = DateTime::parse_from_rfc3339(&started)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(now);
        let resets_at = started + Duration::seconds(window_seconds);
        let (used, resets_at) = if now >= resets_at {
            (0, now + Duration::seconds(window_seconds))
        } else {
            (used, resets_at)
        };
        items.push(json!({
            "bucket": bucket,
            "used": used,
            "limit": limit,
            "remaining": (limit - used).max(0),
            "window_seconds": window_seconds,
            "resets_at": resets_at.to_rfc3339(),
        }));
    }
    Ok(items)
}

/// Cap for exponential backoff between retries of a deferred operation.
const MAX_BACKOFF_SECONDS: i64 = 3_600;

/// Delay before the next retry after `attempt` consecutive failures:
/// 30s, 1m, 2m, 4m... capped at one hour.
pub fn backoff_seconds(attempt: i64) -> i64 {
    let exp = attempt.clamp(1, 12) as u32;
    (30 * 2_i64.pow(exp - 1)).min(MAX_BACKOFF_SECONDS)
}

/// Persist an operation to run once the quota window reopens, and notify the
/// user. Returns the deferred operation id.
#[allow(clippy::too_many_arguments)]
pub async fn defer(
    state: &AppState,
    account_id: &str,
    user_id: &str,
    provider_type: &str,
    operation: &str,
    payload: &serde_json::Value,
    quota: &QuotaStatus,
    reason: &str,
) -> Result<String, ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();

    // Wait for the window to reopen, plus a small margin so the provider's
    // clock has certainly rolled over.
    let available_at = quota.resets_at + Duration::seconds(30);

    // Replace an existing pending deferral for the same operation payload so
    // repeated attempts do not queue duplicates.
    let existing: Option<String> = sqlx::query(
        "SELECT id FROM deferred_operations
         WHERE integration_account_id = ? AND bucket = ? AND operation = ?
           AND payload_json = ? AND status = 'pending'",
    )
    .bind(account_id)
    .bind(&quota.bucket)
    .bind(operation)
    .bind(payload.to_string())
    .fetch_optional(&state.db)
    .await?
    .and_then(|r| r.try_get("id").ok());

    if let Some(existing_id) = existing {
        // Refresh the schedule in case the reset time moved.
        sqlx::query(
            "UPDATE deferred_operations SET available_at = ?, last_error = ?, updated_at = ? WHERE id = ?",
        )
        .bind(available_at.to_rfc3339())
        .bind(reason)
        .bind(now.to_rfc3339())
        .bind(&existing_id)
        .execute(&state.db)
        .await?;
        return Ok(existing_id);
    }

    sqlx::query(
        "INSERT INTO deferred_operations
            (id, integration_account_id, user_id, bucket, operation, payload_json, status,
             available_at, attempt_count, last_error, backoff_seconds, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, 0, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(account_id)
    .bind(user_id)
    .bind(&quota.bucket)
    .bind(operation)
    .bind(payload.to_string())
    .bind(available_at.to_rfc3339())
    .bind(reason)
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&state.db)
    .await?;

    // Tell the user once: the work is queued, not lost.
    let notifications = super::notifications::NotificationService::new(state);
    let notification_id = notifications
        .create_with_actions(
            user_id,
            "rate_limit",
            &format!("{provider_type} limit reached — {operation} queued"),
            &format!(
                "{reason} Auren queued this operation and will run it automatically at {}.",
                available_at.to_rfc3339(),
            ),
            super::notifications::NotificationOptions {
                severity: "warning",
                actions: vec![super::notifications::NotificationAction {
                    id: "retry_now".into(),
                    label: "Run now".into(),
                    style: "primary".into(),
                    kind: "agent".into(),
                    payload: json!({
                        "prompt": format!(
                            "Run the deferred {provider_type} operation '{operation}' now (deferred operation {id})."
                        ),
                        "deferred_operation_id": id,
                    }),
                }],
            },
        )
        .await
        .map_err(ApiError::from)?;

    sqlx::query("UPDATE deferred_operations SET notification_id = ? WHERE id = ?")
        .bind(&notification_id)
        .bind(&id)
        .execute(&state.db)
        .await?;

    Ok(id)
}

/// A deferred operation that is due.
#[derive(Debug)]
pub struct DueOperation {
    pub id: String,
    pub account_id: String,
    pub user_id: String,
    pub bucket: String,
    pub operation: String,
    pub payload: serde_json::Value,
    pub attempt_count: i64,
}

/// Claim due deferred operations (pending and past `available_at`).
pub async fn due_operations(state: &AppState, limit: i64) -> Result<Vec<DueOperation>, ApiError> {
    let now = Utc::now().to_rfc3339();
    let rows = sqlx::query(
        "SELECT id, integration_account_id, user_id, bucket, operation, payload_json, attempt_count
         FROM deferred_operations
         WHERE status = 'pending' AND available_at <= ?
         ORDER BY available_at LIMIT ?",
    )
    .bind(&now)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| DueOperation {
            id: r.get("id"),
            account_id: r.get("integration_account_id"),
            user_id: r.get("user_id"),
            bucket: r.get("bucket"),
            operation: r.get("operation"),
            payload: serde_json::from_str(&r.get::<String, _>("payload_json")).unwrap_or(json!({})),
            attempt_count: r.get("attempt_count"),
        })
        .collect())
}

/// Resolve a deferred operation: success clears the queue entry and tells the
/// user it ran.
pub async fn complete(state: &AppState, operation: &DueOperation, result: &serde_json::Value) {
    let now = Utc::now().to_rfc3339();
    if let Err(e) = sqlx::query(
        "UPDATE deferred_operations SET status = 'completed', updated_at = ?, last_error = NULL WHERE id = ?",
    )
    .bind(&now)
    .bind(&operation.id)
    .execute(&state.db)
    .await
    {
        tracing::warn!(error = %e, "failed to mark deferred operation completed");
        return;
    }
    let notifications = super::notifications::NotificationService::new(state);
    let _ = notifications
        .create_with_actions(
            &operation.user_id,
            "deferred_completed",
            &format!("Queued operation completed: {}", operation.operation),
            &format!(
                "The {} operation that was waiting for the rate limit has finished.",
                operation.operation
            ),
            super::notifications::NotificationOptions {
                severity: "success",
                ..Default::default()
            },
        )
        .await;
    tracing::info!(operation = %operation.id, result = %result, "deferred operation completed");
}

/// Reschedule a deferred operation after a failure, using capped exponential
/// backoff. Exhausted attempts are marked failed and reported to the user.
pub async fn retry_later(state: &AppState, operation: &DueOperation, error: &str) {
    let attempt = operation.attempt_count + 1;
    let now = Utc::now();

    if attempt >= 5 {
        let _ = sqlx::query(
            "UPDATE deferred_operations SET status = 'failed', attempt_count = ?, last_error = ?, updated_at = ? WHERE id = ?",
        )
        .bind(attempt)
        .bind(error)
        .bind(now.to_rfc3339())
        .bind(&operation.id)
        .execute(&state.db)
        .await;
        let notifications = super::notifications::NotificationService::new(state);
        let _ = notifications
            .create_with_actions(
                &operation.user_id,
                "deferred_failed",
                &format!("Queued operation failed: {}", operation.operation),
                error,
                super::notifications::NotificationOptions {
                    severity: "error",
                    actions: vec![super::notifications::NotificationAction {
                        id: "retry".into(),
                        label: "Ask the agent to retry".into(),
                        style: "primary".into(),
                        kind: "agent".into(),
                        payload: json!({
                            "prompt": format!(
                                "The deferred {} operation failed with: {error}. Investigate and retry it.",
                                operation.operation
                            ),
                        }),
                    }],
                },
            )
            .await;
        return;
    }

    let delay = backoff_seconds(attempt);
    let _ = sqlx::query(
        "UPDATE deferred_operations SET attempt_count = ?, last_error = ?, backoff_seconds = ?,
            available_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(attempt)
    .bind(error)
    .bind(delay)
    .bind((now + Duration::seconds(delay)).to_rfc3339())
    .bind(now.to_rfc3339())
    .bind(&operation.id)
    .execute(&state.db)
    .await;
    tracing::warn!(
        operation = %operation.id,
        attempt,
        retry_in_seconds = delay,
        error,
        "deferred operation failed; backing off"
    );
}

/// Cancel deferred operations bound to an account (used when it is
/// disconnected, so a revoked token cannot keep generating retries).
pub async fn cancel_for_account(state: &AppState, account_id: &str) -> Result<u64, ApiError> {
    let now = Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE deferred_operations SET status = 'cancelled', updated_at = ?
         WHERE integration_account_id = ? AND status = 'pending'",
    )
    .bind(&now)
    .bind(account_id)
    .execute(&state.db)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_seconds(1), 30);
        assert_eq!(backoff_seconds(2), 60);
        assert_eq!(backoff_seconds(3), 120);
        assert_eq!(backoff_seconds(4), 240);
        // Capped at one hour no matter how many attempts.
        assert_eq!(backoff_seconds(9), MAX_BACKOFF_SECONDS);
        assert_eq!(backoff_seconds(100), MAX_BACKOFF_SECONDS);
    }

    #[test]
    fn instagram_operations_are_metered() {
        let publish = policy_for("instagram", "publish_media").unwrap();
        assert_eq!(publish.bucket, "publish");
        assert_eq!(publish.limit, 50);
        assert_eq!(publish.window_seconds, 86_400);

        let messages = policy_for("instagram", "send_message").unwrap();
        assert_eq!(messages.bucket, "messages");
        assert_eq!(messages.limit, 200);

        // Reads are metered too, but under their own bucket.
        let reads = policy_for("instagram", "list_media").unwrap();
        assert_eq!(reads.bucket, "reads");

        // Providers without published quotas are not metered.
        assert!(policy_for("gmail", "send_message").is_none());
    }
}
