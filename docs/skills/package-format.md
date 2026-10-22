# Skill Package Format

## Overview

A skill is distributed as a ZIP archive containing a single top-level directory with a `skill.json` manifest.

```text
gmail-monitor/
├── skill.json
├── SKILL.md
├── tools/
│   ├── search_messages.json
│   └── summarize_messages.json
├── resources/
│   └── classification-guidelines.md
└── tests/
    └── examples.json
```

## Required files

| Path | Required | Purpose |
|---|---|---|
| `skill.json` | Yes | Manifest (see [manifest-schema.json](manifest-schema.json)) |
| `SKILL.md` | Yes | Human-readable description shown in the UI |
| `tools/*.json` | If tools declared | Tool definitions with JSON Schema inputs |
| `resources/*` | No | Static resources |
| `tests/examples.json` | No | Example invocations for validation |

## Archive rules

The ZIP is treated as untrusted input. Before extraction the archive must pass validation:

- Rejected: absolute paths, `..` path traversal entries, symbolic links, duplicate paths, ambiguous filenames.
- Limits (configurable, safe defaults): number of entries, total uncompressed size, per-file size, and compression ratio (protection against zip bombs).
- Unsupported compression methods and malformed archives are rejected.
- Unexpected executable artifacts (binaries, scripts) are rejected for declarative skills.

## Installation

- Uploads go through the authenticated `POST /api/v1/skills/uploads` endpoint (`multipart/form-data`), subject to maximum size, timeout, rate limits, and storage quotas.
- Extraction happens into a unique temporary directory, never directly into the active skill directory; every extracted file's resolved destination is verified.
- After validation, the package hash is computed and the package is installed into `/data/skills/<skill-id>/<version>/` and kept immutable.
- Database changes use a transaction; activation is atomic on the filesystem. On failure the previous active version is preserved and temporary files are cleaned up.

## Versioning

- A skill is the logical identity; a skill version is a particular validated package.
- Uploading a newer version validates and installs it independently, then activates it only after success. The previous version is preserved for rollback.
- Re-uploading the same package compares identifier, version, and package hash; duplicate versions are handled explicitly instead of silently overwriting an active version.

## Uninstall

Disable first, prevent new invocations, handle active tasks explicitly, remove the active registration, apply the retention policy to historical metadata, and delete package files only when no longer required. Uninstalling never deletes unrelated files.