-- Actionable notifications, API rate-limit accounting, and deferred retries.
--
-- Notifications gain an optional action so the UI can offer buttons and let
-- the agent act on the notification. Rate limits are tracked per integration
-- account so providers that throttle (Instagram, Gmail...) can defer work
-- instead of failing it.

-- ---------------------------------------------------------------------------
-- Notification actions
-- ---------------------------------------------------------------------------

-- `actions_json` holds an array of { id, label, style, kind, payload }.
-- `kind` is one of: "agent" (the agent acts on it), "url" (open a link),
-- "dismiss". `acted_at`/`action_id` record the chosen action.
ALTER TABLE notifications ADD COLUMN actions_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE notifications ADD COLUMN action_id TEXT;
ALTER TABLE notifications ADD COLUMN acted_at TEXT;
-- Notification severity drives the visual treatment in the UI.
ALTER TABLE notifications ADD COLUMN severity TEXT NOT NULL DEFAULT 'info'
    CHECK (severity IN ('info', 'success', 'warning', 'error'));

CREATE INDEX idx_notifications_user_unread ON notifications(user_id, read_at);

-- ---------------------------------------------------------------------------
-- Provider rate limits
-- ---------------------------------------------------------------------------

-- Rolling-window accounting for provider quotas. `window_seconds` mirrors the
-- provider's published window (e.g. 3600 for hourly, 86400 for daily).
CREATE TABLE rate_limit_usage (
    id                    TEXT PRIMARY KEY,
    integration_account_id TEXT NOT NULL REFERENCES integration_accounts(id) ON DELETE CASCADE,
    -- Logical bucket, e.g. 'publish', 'messages', 'api'.
    bucket                TEXT NOT NULL,
    window_seconds        INTEGER NOT NULL,
    used                  INTEGER NOT NULL DEFAULT 0,
    limit_value           INTEGER NOT NULL,
    window_started_at     TEXT NOT NULL,
    updated_at            TEXT NOT NULL,
    UNIQUE (integration_account_id, bucket)
);

-- Deferred operations waiting for a rate limit window to reopen. The worker
-- retries with the provider's reset time; nothing polls in a tight loop.
CREATE TABLE deferred_operations (
    id                    TEXT PRIMARY KEY,
    integration_account_id TEXT NOT NULL REFERENCES integration_accounts(id) ON DELETE CASCADE,
    user_id               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bucket                TEXT NOT NULL,
    operation             TEXT NOT NULL,
    payload_json          TEXT NOT NULL DEFAULT '{}',
    status                TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    -- Earliest moment the operation may run again.
    available_at          TEXT NOT NULL,
    attempt_count         INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    -- Backoff for transient failures, independent of quota resets.
    backoff_seconds       INTEGER NOT NULL DEFAULT 0,
    notification_id       TEXT REFERENCES notifications(id) ON DELETE SET NULL,
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE INDEX idx_deferred_operations_due ON deferred_operations(status, available_at);
CREATE INDEX idx_deferred_operations_account ON deferred_operations(integration_account_id, status);
