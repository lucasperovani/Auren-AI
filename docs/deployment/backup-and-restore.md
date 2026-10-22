# Backup and Restore

## What to back up

| Item | Location | Notes |
|---|---|---|
| SQLite database | `/data/auren.db` | Includes audit events; WAL/SHM sidecars exist while running |
| Skill packages | `/data/skills/` | Referenced by path in the database; back up together with the database |
| Credential encryption key | Outside the data directory (environment or key file) | Without it, encrypted credentials cannot be decrypted |

## Backup procedure

1. Take a consistent database snapshot. Prefer the `sqlite3` `.backup` command or `VACUUM INTO`, with the server **stopped**, or use the online backup API while it runs:

   ```sh
   # With the server stopped (or read-only):
   sqlite3 /data/auren.db ".backup '/backups/auren-$(date +%F).db'"
   # or
   sqlite3 /data/auren.db "VACUUM INTO '/backups/auren-$(date +%F).db'"
   ```

2. Back up the `skills/` directory after the database snapshot so paths and files stay consistent.
3. Store the credential encryption key separately, with restricted access.

> **WAL sidecar warning:** while the server is running, recent writes live in `auren.db-wal` (and `auren.db-shm`). Copying only the main `auren.db` file from a live server yields an incomplete, potentially corrupt snapshot. Always use `.backup`/`VACUUM INTO`/the online backup API, or stop the server first.

## Restore procedure

1. Stop the server.
2. Restore the database snapshot and the `skills/` directory.
3. Restore the credential encryption key.
4. Start the server; migrations run automatically and the job queue recovers (expired leases are released).

## Retention

Keep daily snapshots for at least 30 days, or per your deployment policy. Audit events are part of the database snapshot and are never modified.
