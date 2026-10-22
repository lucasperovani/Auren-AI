# Configuration

Configuration is provided through environment variables, loaded at startup by `AppConfig::from_env` (`crates/auren-server/src/config.rs`).

## Environment variables

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `AUREN_DATABASE_URL` | No | `sqlite://data/auren.db?mode=rwc` | SQLite database location (`mode=rwc` creates the file on first run) |
| `AUREN_BIND_ADDR` | No | `127.0.0.1:8080` | HTTP bind address. Keep it on localhost when behind a reverse proxy |
| `AUREN_SKILLS_DIR` | No | `data/skills` | Directory where validated skill packages are installed |
| `AUREN_MAX_UPLOAD_BYTES` | No | `52428800` (50 MiB) | Maximum accepted skill upload size in bytes |
| `AUREN_SESSION_SECRET` | **Yes (production)** | *(empty)* | Secret used to hash session tokens (HMAC-style pepper). Generate with `openssl rand -hex 32` |
| `RUST_LOG` | No | `info,tower_http=debug` | Tracing filter (`tracing_subscriber::EnvFilter`) |

## Session secret

- `AUREN_SESSION_SECRET` must be set in production. Session tokens are stored hashed; the secret is the pepper that makes the hashes unverifiable to an attacker with database access alone.
- **Rotating the secret invalidates all existing sessions** — users must log in again. Plan rotation accordingly.
- Do not commit the secret to source control; inject it through your secret manager.

## Credential encryption key

The key used to encrypt stored credentials (API keys, OAuth tokens) is kept **separate from the database** — provided via the environment or a key file — never stored alongside the encrypted data. See the [security model](../architecture/security-model.md). Losing this key makes encrypted credentials unrecoverable.

## Runtime settings

User-facing runtime settings live in the `app_settings` table and are exposed through `/api/v1/workspace/config` and related endpoints; they are not environment variables.

## Related

- [Docker](docker.md) — mounting the data volume.
- [Workspace](../architecture/workspace.md) — changing host mounts may require a container restart.
