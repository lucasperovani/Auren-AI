# Application Lifecycle

## Startup

1. Load configuration from environment variables (see [Configuration](../deployment/configuration.md)).
2. Open the SQLite connection pool with foreign-key enforcement and WAL mode.
3. Run pending versioned migrations.
4. Load installed skills from persistent storage and rebuild the in-memory tool registry (skill installation survives restarts).
5. Recover the job queue: expired leases are released and `leased` jobs return to `pending`.
6. Bind the HTTP listener and begin serving.

## Shutdown

1. Stop accepting new HTTP connections.
2. Stop claiming new jobs from the queue.
3. Allow in-flight steps to reach a safe checkpoint; uncertain outcomes are recorded and reconciled on the next start.
4. Release active leases.

## First-run setup

- The API exposes `GET /api/v1/auth/setup-status`; when no user exists, `POST /api/v1/auth/setup` creates the initial administrator.
- No default administrator password exists. Setup is refused once any user exists.

## Runtime states

| Component | States |
|---|---|
| Agent | `active`, `paused`, `archived` |
| Task | `draft`, `active`, `paused`, `completed`, `cancelled`, `failed` |
| Task run | `pending`, `queued`, `running`, `paused`, `completed`, `failed`, `cancelled` |
| Queued job | `pending`, `leased`, `done`, `failed`, `cancelled` |
| Skill upload | `uploaded`, `validating`, `extracting`, `installing`, `registering`, `installed`, `failed`, `cancelled` |