# Migrations

Migrations are plain SQL files in [`migrations/`](../../migrations/) at the repository root, embedded into the server binary via `sqlx::migrate!` and applied automatically at startup in filename order.

## Naming

`<version>_<description>.sql`, for example `0001_initial_schema.sql`. Versions are monotonically increasing. Never modify an applied migration; add a new one.

## Applying manually

```sh
sqlx migrate run --database-url sqlite://data/auren.db
```

Or simply start the server — pending migrations run before the listener binds.

## Rules

- Every migration runs inside a transaction (SQLx default for SQLite).
- Foreign keys are enforced at connection time; migrations must keep the schema consistent with enforcement on.
- New tables must define nullability, unique constraints, foreign-key deletion behavior, indexes for frequent queries, and status enumerations up front.
- Whenever a migration changes the schema, update the [data dictionary](data-dictionary.md) and the [ER diagram](er-diagram.md) in the same change.

## Current migrations

| Version | File | Contents |
|---|---|---|
| 0001 | `0001_initial_schema.sql` | All core tables: users, sessions, llm_providers, llm_models, agents, conversations, messages, tasks, task_runs, task_steps, scheduled_jobs, queued_jobs, integrations, integration_accounts, encrypted_credentials, skills, skill_versions, skill_tools, agent_skills, agent_tool_permissions, skill_uploads, approval_requests, approval_decisions, memories, notifications, audit_events, app_settings |