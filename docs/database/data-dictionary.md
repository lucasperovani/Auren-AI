# Data Dictionary

Conventions: all primary keys are `TEXT` UUIDv4; timestamps are `TEXT` RFC 3339 UTC; booleans are `INTEGER` 0/1; enumerations are `TEXT` with CHECK constraints; JSON payloads are `TEXT`.

## users

Local users. The initial product supports one administrator, but the schema allows multiple users.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| email | TEXT | No | UNIQUE (case-insensitive via `COLLATE NOCASE`) |
| display_name | TEXT | No | |
| password_hash | TEXT | No | Argon2id PHC string |
| is_admin | INTEGER | No | 0/1 |
| created_at / updated_at | TEXT | No | |

## sessions

Authenticated sessions. Stores a keyed SHA-256 hash of the session token, never the plaintext token.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, ON DELETE CASCADE |
| token_hash | TEXT | No | UNIQUE |
| user_agent | TEXT | Yes | |
| expires_at | TEXT | No | |
| created_at | TEXT | No | |

Indexes: `idx_sessions_user_id (user_id)`, `idx_sessions_expires_at (expires_at)`.

## llm_providers

Configured LLM providers. API keys are stored as authenticated-encrypted blobs with a nonce and key version; the key is kept outside the database.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| name | TEXT | No | |
| provider_type | TEXT | No | `openai_compatible` \| `anthropic` \| `ollama` \| `custom` |
| base_url | TEXT | No | Validated against SSRF policy |
| encrypted_api_key | BLOB | Yes | Null for keyless local providers |
| key_nonce | BLOB | Yes | |
| key_version | INTEGER | No | Default 1 |
| status | TEXT | No | `active` \| `disabled` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_llm_providers_user_id (user_id)`.

## llm_models

Models offered by a provider. `capabilities_json` records declared or discovered capabilities (tool calling, structured output, streaming).

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| provider_id | TEXT | No | FK → llm_providers, CASCADE |
| model_name | TEXT | No | UNIQUE with provider_id |
| capabilities_json | TEXT | No | Default `{}` |
| created_at | TEXT | No | |

## agents

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| name | TEXT | No | |
| instructions | TEXT | No | Default `''` |
| llm_provider_id | TEXT | Yes | FK → llm_providers, ON DELETE SET NULL |
| model_id | TEXT | Yes | FK → llm_models, ON DELETE SET NULL |
| status | TEXT | No | `active` \| `paused` \| `archived` |
| settings_json | TEXT | No | Default `{}` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_agents_user_id (user_id)`.

## conversations

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| agent_id | TEXT | No | FK → agents, CASCADE |
| title | TEXT | No | Default `''` |
| status | TEXT | No | `active` \| `archived` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_conversations_user_id`, `idx_conversations_agent_id`.

## messages

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| conversation_id | TEXT | No | FK → conversations, CASCADE |
| role | TEXT | No | `user` \| `assistant` \| `system` \| `tool` |
| content | TEXT | No | |
| metadata_json | TEXT | No | Default `{}` |
| created_at | TEXT | No | |

Indexes: `idx_messages_conversation_id (conversation_id, created_at)`.

## tasks

The persistent user objective.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| agent_id | TEXT | No | FK → agents, ON DELETE RESTRICT (history preserved) |
| conversation_id | TEXT | Yes | FK → conversations, SET NULL |
| title | TEXT | No | |
| objective | TEXT | No | |
| status | TEXT | No | `draft` \| `active` \| `paused` \| `completed` \| `cancelled` \| `failed` |
| schedule_json | TEXT | Yes | Present for recurring tasks |
| timezone | TEXT | No | Default `UTC` |
| plan_json | TEXT | Yes | |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_tasks_user_id`, `idx_tasks_agent_id`, `idx_tasks_status`.

## task_runs

One attempt to execute a task.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_id | TEXT | No | FK → tasks, CASCADE |
| status | TEXT | No | `pending` \| `queued` \| `running` \| `paused` \| `completed` \| `failed` \| `cancelled` |
| trigger | TEXT | No | `manual` \| `schedule` \| `retry` |
| scheduled_at | TEXT | Yes | |
| started_at / finished_at | TEXT | Yes | |
| result_json | TEXT | Yes | |
| error_message | TEXT | Yes | |
| created_at | TEXT | No | |

Indexes: `idx_task_runs_task_id (task_id, created_at)`, `idx_task_runs_status`.

## task_steps

Ordered tool invocations within a run.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_run_id | TEXT | No | FK → task_runs, CASCADE |
| skill_tool_id | TEXT | Yes | FK → skill_tools, SET NULL |
| integration_account_id | TEXT | Yes | FK → integration_accounts, SET NULL |
| step_index | INTEGER | No | UNIQUE with task_run_id |
| status | TEXT | No | `pending` \| `running` \| `completed` \| `failed` \| `skipped` \| `awaiting_approval` |
| input_json | TEXT | No | Default `{}` |
| output_json | TEXT | Yes | |
| error_message | TEXT | Yes | |
| created_at | TEXT | No | |

Indexes: `idx_task_steps_run_id (task_run_id, step_index)`.

## scheduled_jobs

Persistent schedule per recurring task. UNIQUE on `task_id` prevents duplicate schedules.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_id | TEXT | No | FK → tasks, CASCADE, UNIQUE |
| next_run_at | TEXT | No | |
| schedule_json | TEXT | No | |
| status | TEXT | No | `active` \| `paused` \| `cancelled` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_scheduled_jobs_next_run_at (next_run_at) WHERE status = 'active'` (partial).

## queued_jobs

Persistent job queue with lease-based claiming.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_run_id | TEXT | No | FK → task_runs, CASCADE |
| job_type | TEXT | No | `task_run` \| `reconcile` |
| status | TEXT | No | `pending` \| `leased` \| `done` \| `failed` \| `cancelled` |
| available_at | TEXT | No | Earliest claim time |
| lease_expires_at | TEXT | Yes | Set when leased |
| lease_owner | TEXT | Yes | Worker identifier |
| attempt_count | INTEGER | No | Default 0 |
| last_error | TEXT | Yes | |
| created_at | TEXT | No | |

Indexes: `idx_queued_jobs_claim (status, available_at)`.

## integrations

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| provider_type | TEXT | No | Integration type identifier |
| display_name | TEXT | No | |
| status | TEXT | No | `connected` \| `disconnected` \| `error` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_integrations_user_id`.

## integration_accounts

A provider may have multiple connected accounts, each with independent credentials and scopes.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| integration_id | TEXT | No | FK → integrations, CASCADE |
| external_account_id | TEXT | No | UNIQUE with integration_id |
| display_name | TEXT | No | Default `''` |
| granted_scopes_json | TEXT | No | Default `[]` |
| status | TEXT | No | `active` \| `expired` \| `revoked` \| `error` |
| created_at / updated_at | TEXT | No | |

## encrypted_credentials

Authenticated-encrypted credentials. The encryption key is kept separate from the encrypted data (environment or key file, never the database).

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| integration_account_id | TEXT | No | FK → integration_accounts, CASCADE, UNIQUE (one credential set per account) |
| ciphertext | BLOB | No | |
| nonce | BLOB | No | |
| key_version | INTEGER | No | Default 1; supports key rotation |
| expires_at | TEXT | Yes | |
| updated_at | TEXT | No | |

## skills

Logical identity of an installed skill.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK (skill id from manifest) |
| name | TEXT | No | |
| status | TEXT | No | `enabled` \| `disabled` \| `uninstalled` |
| active_version_id | TEXT | Yes | FK → skill_versions, ON DELETE SET NULL |
| created_at / updated_at | TEXT | No | |

## skill_versions

A particular validated package. UNIQUE `(skill_id, version)`; updating a skill installs a new version and never silently overwrites the previous one.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| skill_id | TEXT | No | FK → skills, CASCADE |
| version | TEXT | No | |
| manifest_json | TEXT | No | Validated manifest |
| package_hash | TEXT | No | Cryptographic hash of the validated package |
| install_path | TEXT | No | Versioned directory, immutable after validation |
| status | TEXT | No | `installed` \| `active` \| `superseded` \| `failed` |
| installed_at | TEXT | No | |

Indexes: `idx_skill_versions_skill_id`.

## skill_tools

Tools declared by a specific skill version, so execution records remain understandable after updates.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| skill_version_id | TEXT | No | FK → skill_versions, CASCADE |
| tool_name | TEXT | No | UNIQUE with skill_version_id |
| description | TEXT | No | Default `''` |
| input_schema_json | TEXT | No | JSON Schema for arguments |
| risk_class | TEXT | No | `low` \| `medium` \| `high` \| `critical` |
| implementation_type | TEXT | No | `declarative` \| `builtin_ref` |
| implementation_ref | TEXT | Yes | Registered tool being referenced |

Indexes: `idx_skill_tools_version_id`.

## agent_skills

Which skill versions an agent has enabled. UNIQUE `(agent_id, skill_version_id)`.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| agent_id | TEXT | No | FK → agents, CASCADE |
| skill_version_id | TEXT | No | FK → skill_versions, CASCADE |
| enabled | INTEGER | No | 0/1 |
| installed_at | TEXT | No | |

Indexes: `idx_agent_skills_agent_id`.

## agent_tool_permissions

Agent-level policy per tool: `allow`, `deny`, or `ask`. UNIQUE `(agent_id, skill_tool_id)`.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| agent_id | TEXT | No | FK → agents, CASCADE |
| skill_tool_id | TEXT | No | FK → skill_tools, CASCADE |
| policy | TEXT | No | `allow` \| `deny` \| `ask` |
| created_at / updated_at | TEXT | No | |

## skill_uploads

Installation state machine (spec section 6).

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK (upload identifier) |
| user_id | TEXT | No | FK → users, CASCADE |
| skill_id | TEXT | Yes | Set after manifest validation |
| version | TEXT | Yes | |
| package_hash | TEXT | Yes | |
| status | TEXT | No | `uploaded` \| `validating` \| `extracting` \| `installing` \| `registering` \| `installed` \| `failed` \| `cancelled` |
| error_message | TEXT | Yes | |
| file_size | INTEGER | No | |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_skill_uploads_user_id`.

## approval_requests

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_run_id | TEXT | Yes | FK → task_runs, CASCADE |
| skill_tool_id | TEXT | Yes | FK → skill_tools, SET NULL |
| operation_hash | TEXT | No | Hash of the canonical operation payload |
| operation_json | TEXT | No | Immutable canonical payload |
| status | TEXT | No | `pending` \| `approved` \| `rejected` \| `cancelled` \| `expired` |
| expires_at | TEXT | No | Time-limited |
| created_at | TEXT | No | |

Indexes: `idx_approval_requests_status (status, expires_at)`.

## approval_decisions

UNIQUE on `approval_request_id` prevents a single decision from being applied to unrelated operations.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| approval_request_id | TEXT | No | FK → approval_requests, CASCADE, UNIQUE |
| user_id | TEXT | No | FK → users, ON DELETE RESTRICT |
| decision | TEXT | No | `approved` \| `rejected` |
| comment | TEXT | Yes | |
| decided_at | TEXT | No | |

Indexes: `idx_approval_decisions_user_id`.

## memories

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| agent_id | TEXT | Yes | FK → agents, SET NULL |
| task_id | TEXT | Yes | FK → tasks, SET NULL |
| category | TEXT | No | Default `general` |
| content | TEXT | No | |
| source_json | TEXT | No | Default `{}` |
| created_at / updated_at | TEXT | No | |

Indexes: `idx_memories_user_id (user_id, category)`.

## notifications

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| user_id | TEXT | No | FK → users, CASCADE |
| task_id | TEXT | Yes | FK → tasks, SET NULL |
| task_run_id | TEXT | Yes | FK → task_runs, SET NULL |
| approval_request_id | TEXT | Yes | FK → approval_requests, SET NULL |
| notification_type | TEXT | No | |
| title | TEXT | No | |
| body | TEXT | No | Default `''` |
| read_at | TEXT | Yes | Reading never authorizes an operation |
| created_at | TEXT | No | |

Indexes: `idx_notifications_user_id (user_id, created_at)`, `idx_notifications_unread (user_id) WHERE read_at IS NULL` (partial).

## audit_events

Append-only at the application level. Never contains secrets.

| Column | Type | Null | Notes |
|---|---|---|---|
| id | TEXT | No | PK |
| task_run_id | TEXT | Yes | FK → task_runs, SET NULL |
| event_type | TEXT | No | |
| actor_type | TEXT | No | `user` \| `agent` \| `system` |
| actor_id | TEXT | Yes | |
| details_json | TEXT | No | Default `{}` |
| created_at | TEXT | No | |

Indexes: `idx_audit_events_created_at`, `idx_audit_events_run_id`.

## app_settings

| Column | Type | Null | Notes |
|---|---|---|---|
| key | TEXT | No | PK |
| value_json | TEXT | No | |
| updated_at | TEXT | No | |

## Retention rules

- `sessions`: expired rows may be deleted opportunistically.
- `queued_jobs`: `done` rows may be pruned after a configurable period.
- `skill_uploads`: failed/cancelled uploads may be pruned; installed history is retained for audit.
- `audit_events`: retained per deployment policy; never modified.
- `messages`, `task_runs`, `task_steps`: retained with their parent conversation/task unless the user deletes the parent.

## Transaction boundaries

- Schedule tick: update `scheduled_jobs` + insert `queued_jobs` in one transaction.
- Job claim: conditional `UPDATE ... WHERE status = 'pending'` then read, in one transaction.
- Task plan confirmation: create `tasks` + `scheduled_jobs` in one transaction.
- Skill activation: update `skills.active_version_id` + version statuses in one transaction, with an atomic filesystem swap.
- Password change: update `users` + delete `sessions` in one transaction.