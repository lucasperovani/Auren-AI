-- Auren-AI initial schema (spec: docs/PROMPT.md section 2)
-- All timestamps are stored as TEXT in ISO 8601 UTC (RFC 3339).
-- All identifiers are UUIDv4 strings.
-- Foreign-key enforcement is enabled at connection time (see db::connect).

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- Users and authentication
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    user_agent TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_sessions_user_id ON sessions(user_id);
CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);

-- ---------------------------------------------------------------------------
-- LLM providers and models
-- ---------------------------------------------------------------------------

CREATE TABLE llm_providers (
    id                TEXT PRIMARY KEY,
    user_id           TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    provider_type     TEXT NOT NULL CHECK (provider_type IN ('openai_compatible', 'anthropic', 'ollama', 'custom')),
    base_url          TEXT NOT NULL,
    encrypted_api_key BLOB, -- nullable: local providers may not need a key
    key_nonce         BLOB,
    key_version       INTEGER NOT NULL DEFAULT 1,
    status            TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE INDEX idx_llm_providers_user_id ON llm_providers(user_id);

CREATE TABLE llm_models (
    id                TEXT PRIMARY KEY,
    provider_id       TEXT NOT NULL REFERENCES llm_providers(id) ON DELETE CASCADE,
    model_name        TEXT NOT NULL,
    capabilities_json TEXT NOT NULL DEFAULT '{}',
    created_at        TEXT NOT NULL,
    UNIQUE (provider_id, model_name)
);

-- ---------------------------------------------------------------------------
-- Agents
-- ---------------------------------------------------------------------------

CREATE TABLE agents (
    id              TEXT PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    instructions    TEXT NOT NULL DEFAULT '',
    llm_provider_id TEXT REFERENCES llm_providers(id) ON DELETE SET NULL,
    model_id        TEXT REFERENCES llm_models(id) ON DELETE SET NULL,
    status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'archived')),
    settings_json   TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE INDEX idx_agents_user_id ON agents(user_id);

-- ---------------------------------------------------------------------------
-- Conversations and messages
-- ---------------------------------------------------------------------------

CREATE TABLE conversations (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    title      TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_conversations_user_id ON conversations(user_id);
CREATE INDEX idx_conversations_agent_id ON conversations(agent_id);

CREATE TABLE messages (
    id              TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    content         TEXT NOT NULL,
    metadata_json   TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL
);

CREATE INDEX idx_messages_conversation_id ON messages(conversation_id, created_at);

-- ---------------------------------------------------------------------------
-- Tasks, runs, steps
-- ---------------------------------------------------------------------------

CREATE TABLE tasks (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id         TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    conversation_id  TEXT REFERENCES conversations(id) ON DELETE SET NULL,
    title            TEXT NOT NULL,
    objective        TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'paused', 'completed', 'cancelled', 'failed')),
    schedule_json    TEXT,
    timezone         TEXT NOT NULL DEFAULT 'UTC',
    plan_json        TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE INDEX idx_tasks_user_id ON tasks(user_id);
CREATE INDEX idx_tasks_agent_id ON tasks(agent_id);
CREATE INDEX idx_tasks_status ON tasks(status);

CREATE TABLE task_runs (
    id            TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'queued', 'running', 'paused', 'completed', 'failed', 'cancelled')),
    trigger       TEXT NOT NULL DEFAULT 'manual' CHECK (trigger IN ('manual', 'schedule', 'retry')),
    scheduled_at  TEXT,
    started_at    TEXT,
    finished_at   TEXT,
    result_json   TEXT,
    error_message TEXT,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_task_runs_task_id ON task_runs(task_id, created_at);
CREATE INDEX idx_task_runs_status ON task_runs(status);

CREATE TABLE task_steps (
    id                     TEXT PRIMARY KEY,
    task_run_id            TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    skill_tool_id          TEXT REFERENCES skill_tools(id) ON DELETE SET NULL,
    integration_account_id TEXT REFERENCES integration_accounts(id) ON DELETE SET NULL,
    step_index             INTEGER NOT NULL,
    status                 TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped', 'awaiting_approval')),
    input_json             TEXT NOT NULL DEFAULT '{}',
    output_json            TEXT,
    error_message          TEXT,
    created_at             TEXT NOT NULL,
    UNIQUE (task_run_id, step_index)
);

CREATE INDEX idx_task_steps_run_id ON task_steps(task_run_id, step_index);

-- ---------------------------------------------------------------------------
-- Scheduling and job queue
-- ---------------------------------------------------------------------------

CREATE TABLE scheduled_jobs (
    id            TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    next_run_at   TEXT NOT NULL,
    schedule_json TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'cancelled')),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (task_id)
);

CREATE INDEX idx_scheduled_jobs_next_run_at ON scheduled_jobs(next_run_at) WHERE status = 'active';

CREATE TABLE queued_jobs (
    id               TEXT PRIMARY KEY,
    task_run_id      TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
    job_type         TEXT NOT NULL DEFAULT 'task_run' CHECK (job_type IN ('task_run', 'reconcile')),
    status           TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'leased', 'done', 'failed', 'cancelled')),
    available_at     TEXT NOT NULL,
    lease_expires_at TEXT,
    lease_owner      TEXT,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       TEXT NOT NULL
);

CREATE INDEX idx_queued_jobs_claim ON queued_jobs(status, available_at);

-- ---------------------------------------------------------------------------
-- Integrations and credentials
-- ---------------------------------------------------------------------------

CREATE TABLE integrations (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_type TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'connected' CHECK (status IN ('connected', 'disconnected', 'error')),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE INDEX idx_integrations_user_id ON integrations(user_id);

CREATE TABLE integration_accounts (
    id                  TEXT PRIMARY KEY,
    integration_id      TEXT NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    external_account_id TEXT NOT NULL,
    display_name        TEXT NOT NULL DEFAULT '',
    granted_scopes_json TEXT NOT NULL DEFAULT '[]',
    status              TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'expired', 'revoked', 'error')),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (integration_id, external_account_id)
);

CREATE TABLE encrypted_credentials (
    id                     TEXT PRIMARY KEY,
    integration_account_id TEXT NOT NULL REFERENCES integration_accounts(id) ON DELETE CASCADE,
    ciphertext             BLOB NOT NULL,
    nonce                  BLOB NOT NULL,
    key_version            INTEGER NOT NULL DEFAULT 1,
    expires_at             TEXT,
    updated_at             TEXT NOT NULL,
    UNIQUE (integration_account_id)
);

-- ---------------------------------------------------------------------------
-- Skills
-- ---------------------------------------------------------------------------

CREATE TABLE skills (
    id                TEXT PRIMARY KEY, -- skill id from manifest
    name              TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'disabled' CHECK (status IN ('enabled', 'disabled', 'uninstalled')),
    active_version_id TEXT REFERENCES skill_versions(id) ON DELETE SET NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE skill_versions (
    id             TEXT PRIMARY KEY,
    skill_id       TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    version        TEXT NOT NULL,
    manifest_json  TEXT NOT NULL,
    package_hash   TEXT NOT NULL,
    install_path   TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'installed' CHECK (status IN ('installed', 'active', 'superseded', 'failed')),
    installed_at   TEXT NOT NULL,
    UNIQUE (skill_id, version)
);

CREATE INDEX idx_skill_versions_skill_id ON skill_versions(skill_id);

CREATE TABLE skill_tools (
    id                  TEXT PRIMARY KEY,
    skill_version_id    TEXT NOT NULL REFERENCES skill_versions(id) ON DELETE CASCADE,
    tool_name           TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    input_schema_json   TEXT NOT NULL DEFAULT '{}',
    risk_class          TEXT NOT NULL DEFAULT 'low' CHECK (risk_class IN ('low', 'medium', 'high', 'critical')),
    implementation_type TEXT NOT NULL DEFAULT 'declarative' CHECK (implementation_type IN ('declarative', 'builtin_ref')),
    implementation_ref  TEXT,
    UNIQUE (skill_version_id, tool_name)
);

CREATE INDEX idx_skill_tools_version_id ON skill_tools(skill_version_id);

CREATE TABLE agent_skills (
    id               TEXT PRIMARY KEY,
    agent_id         TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    skill_version_id TEXT NOT NULL REFERENCES skill_versions(id) ON DELETE CASCADE,
    enabled          INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    installed_at     TEXT NOT NULL,
    UNIQUE (agent_id, skill_version_id)
);

CREATE INDEX idx_agent_skills_agent_id ON agent_skills(agent_id);

CREATE TABLE agent_tool_permissions (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    skill_tool_id TEXT NOT NULL REFERENCES skill_tools(id) ON DELETE CASCADE,
    policy        TEXT NOT NULL CHECK (policy IN ('allow', 'deny', 'ask')),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (agent_id, skill_tool_id)
);

CREATE TABLE skill_uploads (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    skill_id      TEXT,
    version       TEXT,
    package_hash  TEXT,
    status        TEXT NOT NULL DEFAULT 'uploaded' CHECK (status IN ('uploaded', 'validating', 'extracting', 'installing', 'registering', 'installed', 'failed', 'cancelled')),
    error_message TEXT,
    file_size     INTEGER NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE INDEX idx_skill_uploads_user_id ON skill_uploads(user_id);

-- ---------------------------------------------------------------------------
-- Approvals
-- ---------------------------------------------------------------------------

CREATE TABLE approval_requests (
    id             TEXT PRIMARY KEY,
    task_run_id    TEXT REFERENCES task_runs(id) ON DELETE CASCADE,
    skill_tool_id  TEXT REFERENCES skill_tools(id) ON DELETE SET NULL,
    operation_hash TEXT NOT NULL,
    operation_json TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected', 'cancelled', 'expired')),
    expires_at     TEXT NOT NULL,
    created_at     TEXT NOT NULL
);

CREATE INDEX idx_approval_requests_status ON approval_requests(status, expires_at);

CREATE TABLE approval_decisions (
    id                 TEXT PRIMARY KEY,
    approval_request_id TEXT NOT NULL REFERENCES approval_requests(id) ON DELETE CASCADE,
    user_id            TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    decision           TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
    comment            TEXT,
    decided_at         TEXT NOT NULL,
    UNIQUE (approval_request_id)
);

CREATE INDEX idx_approval_decisions_user_id ON approval_decisions(user_id);

-- ---------------------------------------------------------------------------
-- Memory, notifications, audit, settings
-- ---------------------------------------------------------------------------

CREATE TABLE memories (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_id     TEXT REFERENCES agents(id) ON DELETE SET NULL,
    task_id      TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    category     TEXT NOT NULL DEFAULT 'general',
    content      TEXT NOT NULL,
    source_json  TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE INDEX idx_memories_user_id ON memories(user_id, category);

CREATE TABLE notifications (
    id                  TEXT PRIMARY KEY,
    user_id             TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id             TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    task_run_id         TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    approval_request_id TEXT REFERENCES approval_requests(id) ON DELETE SET NULL,
    notification_type   TEXT NOT NULL,
    title               TEXT NOT NULL,
    body                TEXT NOT NULL DEFAULT '',
    read_at             TEXT,
    created_at          TEXT NOT NULL
);

CREATE INDEX idx_notifications_user_id ON notifications(user_id, created_at);
CREATE INDEX idx_notifications_unread ON notifications(user_id) WHERE read_at IS NULL;

CREATE TABLE audit_events (
    id           TEXT PRIMARY KEY,
    task_run_id  TEXT REFERENCES task_runs(id) ON DELETE SET NULL,
    event_type   TEXT NOT NULL,
    actor_type   TEXT NOT NULL CHECK (actor_type IN ('user', 'agent', 'system')),
    actor_id     TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL
);

CREATE INDEX idx_audit_events_created_at ON audit_events(created_at);
CREATE INDEX idx_audit_events_run_id ON audit_events(task_run_id);

CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);