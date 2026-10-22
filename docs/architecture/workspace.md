# Workspace

The workspace is the set of host directories the application is explicitly authorized to access for file operations.

## Rules

- Every file operation resolves its target against an explicitly authorized root and rejects traversal outside that root.
- Arbitrary filesystem paths are never accepted through unrestricted API parameters.
- The authorized roots are configured at deployment time (see [Configuration](../deployment/configuration.md)); changing host mounts may require a container restart.
- The API exposes only relative paths under an authorized root (`/api/v1/workspace/files`).

## Layout

```text
/data/            # persistent application data (Docker volume)
├── auren.db      # SQLite database (+ WAL/SHM sidecars)
├── skills/       # installed skill packages: /data/skills/<skill-id>/<version>/
├── uploads/      # temporary upload staging (cleaned after processing)
└── workspace/    # default authorized workspace root
```

Skill directories are immutable after validation; updates install into a new version directory and activation is atomic.