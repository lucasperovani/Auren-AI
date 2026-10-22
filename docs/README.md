# Auren-AI Documentation

Auren-AI is a self-hosted autonomous AI agent platform. This directory is the authoritative documentation for the project.

All documentation, diagrams, API specifications, comments, identifiers, and source code are written in English.

## Index

- [Architecture](architecture/overview.md) — system overview, lifecycle, agent runtime, scheduling, security model, workspace.
- [Database](database/README.md) — ER diagram, data dictionary, migration guide.
- [API](api/README.md) — REST API reference, OpenAPI specification, WebSocket events.
- [Skills](skills/README.md) — skill package format, manifest schema, permissions, authoring guide.
- [Deployment](deployment/docker.md) — Docker, configuration, backup/restore, reverse proxy.
- [Development](development/setup.md) — local setup, testing, contributing.

## Key references

| Document | Purpose |
|---|---|
| [ER diagram](database/er-diagram.md) | Authoritative logical data model |
| [Data dictionary](database/data-dictionary.md) | Every table, column, constraint, and index |
| [OpenAPI spec](api/openapi.yaml) | REST API contract |
| [WebSocket events](api/websocket.md) | Real-time event stream contract |
| [Manifest schema](skills/manifest-schema.json) | JSON Schema for `skill.json` |
| [Security model](architecture/security-model.md) | Trust boundaries and authorization |

## Maintenance rule

Documentation is maintained alongside the implementation. Whenever a database migration, API endpoint, skill format, or significant architectural behavior changes, update the corresponding document in this directory in the same change.