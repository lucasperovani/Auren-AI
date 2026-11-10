-- Kanban staging for tasks.
--
-- `tasks.status` is the *execution* state owned by the scheduler and the
-- runtime (draft/active/paused/completed/cancelled/failed): it decides whether
-- a schedule fires. The board needs an orthogonal *workflow* position, so
-- `stage` tracks where a task sits on the board without touching scheduling.
--
-- Stages: analysis -> todo -> in_progress -> done.
-- `analysis` holds tasks that still need scope/requirements work; `todo` is
-- ready-but-not-running; `in_progress` is actively being worked; `done` is
-- closed. The engine advances stages automatically on run start/finish, and
-- the user can move a card by hand at any time.

ALTER TABLE tasks ADD COLUMN stage TEXT NOT NULL DEFAULT 'todo'
    CHECK (stage IN ('analysis', 'todo', 'in_progress', 'done'));

-- Preserve intent for existing rows: a scheduled task waiting to fire belongs
-- in To Do, a paused/on-demand one still needs planning, and closed work is
-- Done.
UPDATE tasks SET stage = CASE
    WHEN status IN ('completed', 'cancelled') THEN 'done'
    WHEN status = 'active' THEN 'todo'
    WHEN status = 'paused' THEN 'analysis'
    ELSE 'analysis'
END;

-- Board queries sort within a column by recency, and the column filter is the
-- common access path.
CREATE INDEX idx_tasks_stage ON tasks(stage, updated_at);
