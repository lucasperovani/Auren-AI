# Agent Runtime

The agent runtime executes task runs. A **task** is the persistent user objective; a **task run** is one attempt to execute it.

## Execution model

1. A run is created (manually, by a schedule, or as a retry) and enqueued as a `queued_jobs` row.
2. A worker claims the job using a lease (`available_at`, `lease_expires_at`, `lease_owner`).
3. The runtime builds a plan (`plan_json`) and executes it as ordered `task_steps`.
4. Each step invokes a tool through the **permission engine**, which evaluates:
   - agent-level tool permissions (`agent_tool_permissions`),
   - global security policy,
   - the connected integration account and its granted OAuth scopes,
   - the operation arguments,
   - approval requirements (`risk_class` and policy).
5. If approval is required, the step moves to `awaiting_approval`, an `approval_requests` row is created with a canonical operation payload and hash, and the run pauses until a decision arrives.
6. Step results are persisted in `output_json` before the next step starts.

## Model capabilities

Do not assume a model supports tool calling, structured output, or streaming. Capabilities are stored per model in `llm_models.capabilities_json` and discovered or declared explicitly when a provider is configured.

## Failure and cancellation

- Cancellation is cooperative: already-completed external operations are not reversed.
- Uncertain outcomes are recorded and reconciled before retrying; the runtime never reports a false success or failure.
- A failed step marks the run `failed` with `error_message`; the task may be retried, creating a new run.

## Memory

The runtime may read and write `memories` scoped to the user, agent, and task. Memory access never bypasses authorization: an agent can only reference resources it could otherwise access.