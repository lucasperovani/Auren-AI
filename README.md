# Auren-AI

A self-hosted autonomous AI agent platform. Auren-AI runs inside a single Docker container and exposes a web interface where you can connect external LLM providers, install skills, authorize external services, create autonomous agents, schedule recurring tasks using natural language, manage persistent memory, and approve sensitive operations.

## Features

- **Conversational task creation** — request recurring work in plain language; the agent infers schedules, asks clarifying questions, and validates everything in the backend before saving.
- **Persistent scheduler** — tasks run independently of the browser session; the queue survives container restarts with lease-based claiming and retry policies.
- **Skill system** — install capability packages (ZIP) through the web interface; packages are validated before extraction and registered without restarting the application.
- **Approval engine** — sensitive operations (external messages, purchases, destructive changes) require explicit, operation-bound approval; any material parameter change invalidates the approval.
- **Integrations** — connect multiple accounts per provider with least-privilege OAuth scopes; credentials are encrypted at rest and never exposed to the LLM or the frontend.
- **Workspace** — a configurable directory where the agent stores files and artifacts, restricted against path traversal.

## Quick start

### Docker Compose (recommended)

```sh
cp .env.example .env
# Fill in AUREN_SESSION_SECRET (required):
openssl rand -hex 32   # paste the output into .env
docker compose up -d --build
```

Open `http://localhost:8080`, create the administrator account, and configure an LLM provider in Settings.

### Local development

```sh
cargo run -p auren-server
# Server on http://127.0.0.1:8080; frontend served from ./frontend
```

Environment variables:

| Variable | Purpose | Default |
|---|---|---|
| `AUREN_DATABASE_URL` | SQLite database path | `sqlite://data/auren.db?mode=rwc` |
| `AUREN_BIND_ADDR` | HTTP bind address | `127.0.0.1:8080` |
| `AUREN_SESSION_SECRET` | Pepper for session token hashing | *(empty — set in production)* |
| `AUREN_CREDENTIAL_ENCRYPTION_KEY` | Key for credential encryption at rest | derived from the session secret |
| `AUREN_SKILLS_DIR` | Installed skill packages | `data/skills` |
| `AI_WORKSPACE_CONTAINER_PATH` | Agent workspace directory | `data/workspace` |
| `AUREN_MAX_UPLOAD_BYTES` | Skill upload size limit | `52428800` (50 MB) |
| `AUREN_MAX_WORKSPACE_FILE_BYTES` | Per-file workspace limit | `10485760` (10 MB) |

## Workspace configuration

The agent workspace is the only directory where the agent can read and write files. To map a host directory:

1. Set `AI_WORKSPACE_HOST_PATH=./workspace` in `.env` (relative paths resolve against the Compose directory).
2. The container mounts it at `AI_WORKSPACE_CONTAINER_PATH` (default `/workspace`).

Distinct directories inside the container:

| Directory | Purpose |
|---|---|
| `/data` | Application data: SQLite database |
| `/data/skills` | Installed, validated skill packages (immutable per version) |
| `/workspace` | Agent workspace: files, artifacts, task working data |
| `/tmp` | Temporary execution and extraction directories (not persisted) |

## Security

- Passwords are hashed with Argon2id; sessions store only a keyed hash of the token.
- Login attempts are rate limited per account.
- Credentials (LLM API keys, OAuth tokens) are encrypted with AES-256-GCM; the encryption key is configured separately from the database.
- All tool invocations pass through a single permission engine; high/critical-risk tools always require approval regardless of agent configuration.
- Skill ZIPs are treated as untrusted input: path traversal, absolute paths, symlinks, duplicate entries, and zip bombs are rejected before extraction.
- The web research tool enforces SSRF protection and never reaches private/internal network destinations.
- Audit events are append-only and never contain secrets.
- The application runs as a non-root user and never exposes the Docker socket.

For public deployments, place the container behind a reverse proxy with HTTPS. See [docs/deployment/reverse-proxy.md](docs/deployment/reverse-proxy.md).

## Backups

All persistent state lives in the `/data` volume (database) and the workspace volume. Back up with:

```sh
docker compose exec auren sqlite3 /data/auren.db ".backup /data/auren-backup.db"
docker cp auren-ai:/data/auren-backup.db ./backup.db
```

Restore by copying the backup file back to `/data/auren.db` and restarting the container. See [docs/deployment/backup-and-restore.md](docs/deployment/backup-and-restore.md).

## Integrations

| Provider | Status | Notes |
|---|---|---|
| OpenAI-compatible APIs | Supported | Any OpenAI-compatible endpoint, including Ollama (`/v1`) |
| Ollama | Supported | Local models; no API key required |
| Gmail / Google Calendar | Framework ready | Requires a Google Cloud OAuth client; see [docs/api/integrations.md](docs/api/integrations.md) |
| Flight monitoring | Monitoring only | Purchasing requires a supported booking integration and explicit approval |

The application is fully functional without any LLM or integration configured: you can create agents, install skills, and manage tasks; the agent needs an LLM provider to plan and execute runs.

## Documentation

Full documentation lives in [docs/](docs/README.md): architecture, database schema and ER diagram, the complete OpenAPI specification, WebSocket events, skill package format, and deployment guides.

## Development

```sh
cargo test          # run the test suite
cargo clippy        # lint
```

See [docs/development/contributing.md](docs/development/contributing.md) for conventions.

## Known limitations

- Gemini and Anthropic providers are not yet implemented (their wire formats differ from the OpenAI-compatible interface).
- Gmail, Calendar, Photos, Instagram, and flight integrations have the connection framework but require real OAuth developer applications to become operational; unsupported operations are never simulated.
- Executable third-party skills are not supported; skills are declarative and compose validated built-in tools.
- The scheduler is single-instance; running two containers against the same database is not supported.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
