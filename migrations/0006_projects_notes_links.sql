-- Projects, task notes, and links between tasks.
--
-- Projects group tasks so work on one does not interfere with another: the
-- board is scoped by project, and the agent receives the project when it
-- creates or lists tasks. Tasks without a project stay in a single default
-- board, so existing data keeps working.

CREATE TABLE projects (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- Board accent, so projects are visually distinguishable.
    color       TEXT NOT NULL DEFAULT 'blue'
        CHECK (color IN ('blue', 'green', 'purple', 'orange', 'pink', 'gray')),
    status      TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'archived')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX idx_projects_user_id ON projects(user_id, status);

-- `project_id` is nullable: null means the task lives on the default board.
ALTER TABLE tasks ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;

CREATE INDEX idx_tasks_project ON tasks(project_id, stage);

-- Free-form notes the user or the agent writes on a card, kept apart from the
-- objective so the objective stays the stable instruction.
CREATE TABLE task_notes (
    id         TEXT PRIMARY KEY,
    task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    author     TEXT NOT NULL CHECK (author IN ('user', 'agent')),
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_task_notes_task ON task_notes(task_id, created_at);

-- Explicit references between tasks. `blocks` means the target must finish
-- first; `relates` is informational. Stored in one direction only and read
-- back both ways.
CREATE TABLE task_links (
    id           TEXT PRIMARY KEY,
    from_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    to_task_id   TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL DEFAULT 'relates'
        CHECK (kind IN ('blocks', 'relates')),
    created_at   TEXT NOT NULL,
    UNIQUE (from_task_id, to_task_id, kind),
    -- A task cannot reference itself.
    CHECK (from_task_id != to_task_id)
);

CREATE INDEX idx_task_links_from ON task_links(from_task_id);
CREATE INDEX idx_task_links_to ON task_links(to_task_id);

-- When a task was assigned to the agent for immediate execution, and who
-- triggered it. Kept for the activity timeline.
ALTER TABLE task_runs ADD COLUMN assigned_by TEXT;
