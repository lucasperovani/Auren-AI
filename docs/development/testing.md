# Testing

## Running tests

```sh
cargo test                      # all workspace tests
cargo test -p auren-server      # server crate only
cargo test auth                 # a single module
```

## What is covered

- **Unit tests** live next to the code (`#[cfg(test)] mod tests`) and cover pure logic: password hashing (Argon2), session token hashing, pagination cursor encoding/decoding roundtrip, cron schedule evaluation, and validation.
- **Migration tests**: migrations are applied against a temporary database to verify the schema is valid and reproducible; foreign-key enforcement is verified on pooled connections.
- **API tests** exercise handlers through the router with a temporary SQLite database: authentication requirements, ownership enforcement, task lifecycle, task plans, approvals, notifications, and workspace traversal rejection.
- **Service tests** cover the scheduler (cron evaluation, queue claiming, lease recovery, retry/backoff), the permission engine (policies, risk classes, scope checks), the approval binding (operation hash revalidation), and audit redaction.
- **Skill installation tests** cover the full pipeline: valid installation, path traversal rejection, absolute path rejection, duplicate entries, invalid manifests, unknown implementation references, idempotent re-upload, failed updates preserving the previous active version, uninstall cleanup, and restart survival (spec Definition of Done, items 8-16).
- **WebSocket tests** cover the ticket flow (issuance, single use, expiry) and end-to-end event delivery over a real socket.

## Requirements for new code

- New endpoints need tests covering the happy path and authorization (authenticated vs. unauthenticated, cross-user access).
- Security-critical paths must have automated tests before merge: ZIP path traversal and decompression attacks for skill installation, permission enforcement, approval binding, and credential handling (spec Definition of Done, item 9).
- A bug fix must come with a regression test.

## Conventions

- Tests are written in English.
- Prefer fast unit tests over integration tests where logic can be isolated.
