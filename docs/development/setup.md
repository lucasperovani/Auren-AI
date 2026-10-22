# Setup

## Prerequisites

- Rust 1.85+ (install with [rustup](https://rustup.rs)); the workspace enforces `rust-version = 1.85`
- SQLite is bundled via sqlx; no system dependency required

## Build and run

```sh
git clone https://github.com/lucasperovani/Auren-AI
cd Auren-AI
cargo build
AUREN_SESSION_SECRET=$(openssl rand -hex 32) cargo run -p auren-server
```

The server listens on `http://127.0.0.1:8080` by default and applies migrations automatically at startup.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `AUREN_DATABASE_URL` | `sqlite://data/auren.db?mode=rwc` | SQLite database location |
| `AUREN_BIND_ADDR` | `127.0.0.1:8080` | HTTP bind address |
| `AUREN_SKILLS_DIR` | `data/skills` | Skill installation directory |
| `AUREN_MAX_UPLOAD_BYTES` | `52428800` (50 MiB) | Maximum skill upload size |
| `AUREN_SESSION_SECRET` | *(empty)* | Required for login; generate with `openssl rand -hex 32` |
| `RUST_LOG` | `info,tower_http=debug` | Tracing filter |

See [Configuration](../deployment/configuration.md) for the full reference.

## First run

1. `GET /api/v1/auth/setup-status` → `{"setup_required": true}`
2. `POST /api/v1/auth/setup` with email, display name, and password (min 8 chars).
3. `POST /api/v1/auth/login` to obtain a session token.

## Migrations

SQLx migrations in [`migrations/`](../../migrations/) are embedded at build time and applied automatically on server startup — no manual step is needed. See [Migrations](../database/migrations.md) for the rules.

## Project layout

```text
crates/auren-server/src/
├── api/          # Axum handlers and router (auth, health, pagination)
├── auth.rs       # Password and session token hashing
├── config.rs     # Environment variable configuration
├── db/           # SQLite pool setup (WAL, foreign keys) and migrations
├── error.rs      # Error types and HTTP responses
├── state.rs      # Shared application state
└── main.rs       # Entry point
migrations/       # Versioned SQL migrations (embedded at build time)
docs/             # This documentation
```
