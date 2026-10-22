# ER Diagram

The authoritative logical data model. Physical types, indexes, constraints, and nullability are defined in the migrations and the [data dictionary](data-dictionary.md).

```mermaid
erDiagram
    USERS ||--o{ SESSIONS : owns
    USERS ||--o{ AGENTS : configures
    USERS ||--o{ CONVERSATIONS : owns
    USERS ||--o{ TASKS : creates
    USERS ||--o{ INTEGRATIONS : configures
    USERS ||--o{ APPROVAL_DECISIONS : makes
    USERS ||--o{ NOTIFICATIONS : receives
    USERS ||--o{ MEMORIES : owns
    USERS ||--o{ SKILL_UPLOADS : uploads

    AGENTS ||--o{ CONVERSATIONS : participates_in
    AGENTS ||--o{ TASKS : executes
    AGENTS ||--o{ AGENT_SKILLS : enables
    AGENTS ||--o{ AGENT_TOOL_PERMISSIONS : configures
    AGENTS }o--o| LLM_PROVIDERS : uses

    CONVERSATIONS ||--o{ MESSAGES : contains
    CONVERSATIONS ||--o{ TASKS : originates

    TASKS ||--o{ TASK_RUNS : has
    TASKS ||--o| SCHEDULED_JOBS : schedules
    TASKS ||--o{ MEMORIES : references

    TASK_RUNS ||--o{ TASK_STEPS : contains
    TASK_RUNS ||--o{ QUEUED_JOBS : dispatches
    TASK_RUNS ||--o{ APPROVAL_REQUESTS : requires
    TASK_RUNS ||--o{ AUDIT_EVENTS : records

    TASK_STEPS }o--o| SKILL_TOOLS : invokes
    TASK_STEPS }o--o| INTEGRATION_ACCOUNTS : accesses

    LLM_PROVIDERS ||--o{ LLM_MODELS : offers

    INTEGRATIONS ||--o{ INTEGRATION_ACCOUNTS : contains
    INTEGRATION_ACCOUNTS ||--o| ENCRYPTED_CREDENTIALS : authenticates

    SKILLS ||--o{ SKILL_VERSIONS : has
    SKILL_VERSIONS ||--o{ SKILL_TOOLS : declares
    SKILL_VERSIONS ||--o{ AGENT_SKILLS : selected_by

    SKILL_TOOLS ||--o{ AGENT_TOOL_PERMISSIONS : governed_by
    SKILL_TOOLS ||--o{ APPROVAL_REQUESTS : requests

    APPROVAL_REQUESTS ||--o{ APPROVAL_DECISIONS : receives

    NOTIFICATIONS }o--o| TASKS : concerns
    NOTIFICATIONS }o--o| TASK_RUNS : concerns
    NOTIFICATIONS }o--o| APPROVAL_REQUESTS : concerns

    USERS {
        string id PK
        string email
        string display_name
        string password_hash
        int is_admin
        string created_at
        string updated_at
    }

    SESSIONS {
        string id PK
        string user_id FK
        string token_hash
        string user_agent
        string expires_at
        string created_at
    }

    AGENTS {
        string id PK
        string user_id FK
        string name
        string instructions
        string llm_provider_id FK
        string model_id FK
        string status
        string settings_json
        string created_at
        string updated_at
    }

    CONVERSATIONS {
        string id PK
        string user_id FK
        string agent_id FK
        string title
        string status
        string created_at
        string updated_at
    }

    MESSAGES {
        string id PK
        string conversation_id FK
        string role
        string content
        string metadata_json
        string created_at
    }

    TASKS {
        string id PK
        string user_id FK
        string agent_id FK
        string conversation_id FK
        string title
        string objective
        string status
        string schedule_json
        string timezone
        string plan_json
        string created_at
        string updated_at
    }

    TASK_RUNS {
        string id PK
        string task_id FK
        string status
        string trigger
        string scheduled_at
        string started_at
        string finished_at
        string result_json
        string error_message
        string created_at
    }

    TASK_STEPS {
        string id PK
        string task_run_id FK
        string skill_tool_id FK
        string integration_account_id FK
        int step_index
        string status
        string input_json
        string output_json
        string error_message
        string created_at
    }

    SCHEDULED_JOBS {
        string id PK
        string task_id FK
        string next_run_at
        string schedule_json
        string status
        string created_at
        string updated_at
    }

    QUEUED_JOBS {
        string id PK
        string task_run_id FK
        string job_type
        string status
        string available_at
        string lease_expires_at
        string lease_owner
        int attempt_count
        string last_error
        string created_at
    }

    LLM_PROVIDERS {
        string id PK
        string user_id FK
        string name
        string provider_type
        string base_url
        blob encrypted_api_key
        blob key_nonce
        int key_version
        string status
        string created_at
        string updated_at
    }

    LLM_MODELS {
        string id PK
        string provider_id FK
        string model_name
        string capabilities_json
        string created_at
    }

    INTEGRATIONS {
        string id PK
        string user_id FK
        string provider_type
        string display_name
        string status
        string created_at
        string updated_at
    }

    INTEGRATION_ACCOUNTS {
        string id PK
        string integration_id FK
        string external_account_id
        string display_name
        string granted_scopes_json
        string status
        string created_at
        string updated_at
    }

    ENCRYPTED_CREDENTIALS {
        string id PK
        string integration_account_id FK
        blob ciphertext
        blob nonce
        int key_version
        string expires_at
        string updated_at
    }

    SKILLS {
        string id PK
        string name
        string status
        string active_version_id FK
        string created_at
        string updated_at
    }

    SKILL_VERSIONS {
        string id PK
        string skill_id FK
        string version
        string manifest_json
        string package_hash
        string install_path
        string status
        string installed_at
    }

    SKILL_TOOLS {
        string id PK
        string skill_version_id FK
        string tool_name
        string description
        string input_schema_json
        string risk_class
        string implementation_type
        string implementation_ref
    }

    AGENT_SKILLS {
        string id PK
        string agent_id FK
        string skill_version_id FK
        int enabled
        string installed_at
    }

    AGENT_TOOL_PERMISSIONS {
        string id PK
        string agent_id FK
        string skill_tool_id FK
        string policy
        string created_at
        string updated_at
    }

    SKILL_UPLOADS {
        string id PK
        string user_id FK
        string skill_id
        string version
        string package_hash
        string status
        string error_message
        int file_size
        string created_at
        string updated_at
    }

    APPROVAL_REQUESTS {
        string id PK
        string task_run_id FK
        string skill_tool_id FK
        string operation_hash
        string operation_json
        string status
        string expires_at
        string created_at
    }

    APPROVAL_DECISIONS {
        string id PK
        string approval_request_id FK
        string user_id FK
        string decision
        string comment
        string decided_at
    }

    MEMORIES {
        string id PK
        string user_id FK
        string agent_id FK
        string task_id FK
        string category
        string content
        string source_json
        string created_at
        string updated_at
    }

    NOTIFICATIONS {
        string id PK
        string user_id FK
        string task_id FK
        string task_run_id FK
        string approval_request_id FK
        string notification_type
        string title
        string body
        string read_at
        string created_at
    }

    AUDIT_EVENTS {
        string id PK
        string task_run_id FK
        string event_type
        string actor_type
        string actor_id
        string details_json
        string created_at
    }
```

## Differences from the original specification diagram

- `USERS` includes `display_name` and `is_admin` to support the setup flow.
- `AGENTS` includes `settings_json`; `CONVERSATIONS` includes `status`; `MESSAGES` includes `metadata_json`.
- `TASK_RUNS` includes `trigger` to distinguish manual, scheduled, and retry runs.
- `TASK_STEPS` includes `error_message`.
- `QUEUED_JOBS` includes `job_type`, `lease_owner`, and `last_error` for lease-based claiming and reconciliation.
- `LLM_PROVIDERS` stores `key_nonce` and `key_version` alongside `encrypted_api_key` (authenticated encryption metadata) and a `status`.
- `SKILL_TOOLS` includes `implementation_ref` for declarative references to registered tools.
- `SKILL_UPLOADS` was added to persist the installation state machine (spec section 6).
- `APPROVAL_DECISIONS` has a unique constraint on `approval_request_id` so a single decision cannot be applied to unrelated operations.