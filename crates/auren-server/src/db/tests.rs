use crate::db;

/// Migrations must apply cleanly against a fresh database and be idempotent
/// (a second run must be a no-op).
#[tokio::test]
async fn migrations_apply_and_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = db::connect(&url).await.unwrap();

    db::migrate(&pool).await.unwrap();
    // Running again must not fail.
    db::migrate(&pool).await.unwrap();

    // Spot-check that core tables exist.
    for table in ["users", "sessions", "agents", "tasks", "task_runs", "skills", "approval_requests"] {
        let exists: Option<(i64,)> =
            sqlx::query_as(&format!("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '{table}'"))
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(exists.is_some(), "table {table} missing");
    }
}

/// Foreign-key enforcement must be on for pooled connections.
#[tokio::test]
async fn foreign_keys_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = db::connect(&url).await.unwrap();
    db::migrate(&pool).await.unwrap();

    let result = sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at) VALUES ('s1', 'no-user', 'h', '2026-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err(), "FK violation expected");
}