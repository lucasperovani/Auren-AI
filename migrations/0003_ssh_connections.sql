-- SSH connections registered by the agent for remote command execution.
-- No credentials are stored: authentication relies on the server host's
-- key-based SSH setup (agent, default key files, or ssh_config entries).
CREATE TABLE ssh_connections (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    host       TEXT NOT NULL,
    port       INTEGER NOT NULL DEFAULT 22,
    username   TEXT,
    status     TEXT NOT NULL DEFAULT 'connected' CHECK (status IN ('connected', 'disconnected')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_ssh_connections_user_id ON ssh_connections(user_id);