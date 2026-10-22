# Database

Auren-AI uses SQLite with SQLx and versioned SQL migrations.

- Migrations live in [`migrations/`](../../migrations/) at the repository root and are embedded into the server binary at build time.
- Identifiers are UUID strings.
- Timestamps are stored as TEXT in ISO 8601 UTC (RFC 3339).
- Foreign keys are enforced at connection time; WAL mode enables concurrent readers with a single writer.

## Contents

- [ER diagram](er-diagram.md) — authoritative logical data model.
- [Data dictionary](data-dictionary.md) — every table, column, key, index, and constraint.
- [Migrations](migrations.md) — how migrations are structured and applied.

## Conventions

| Convention | Value |
|---|---|
| Primary keys | `TEXT` UUIDv4 |
| Timestamps | `TEXT` RFC 3339 UTC |
| Booleans | `INTEGER` 0/1 with CHECK constraints |
| Enumerations | `TEXT` with CHECK constraints |
| JSON payloads | `TEXT` containing serialized JSON, validated at the application boundary |
| Deletion behavior | `CASCADE` for owned children, `SET NULL` for optional references, `RESTRICT` where history must be preserved |