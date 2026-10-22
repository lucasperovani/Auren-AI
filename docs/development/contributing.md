# Contributing

## Language

All code, comments, identifiers, and documentation are written in English.

## Documentation rule

Documentation is maintained alongside the implementation: whenever a database migration, API endpoint, skill format, or significant architectural behavior changes, update the corresponding document in [docs/](../README.md) in the same change.

## Migration rules

- Migrations live in [`migrations/`](../../migrations/) and are embedded at build time.
- **Never modify an applied migration.** Add a new migration instead.
- Update the [data dictionary](../database/data-dictionary.md) and [ER diagram](../database/er-diagram.md) in the same change.

## API and skill format changes

- Update [openapi.yaml](../api/openapi.yaml) and the relevant endpoint documentation.
- Skill format changes: update [package-format.md](../skills/package-format.md) and bump the manifest schema version if needed.

## Code conventions

- Descriptive names; concise comments only where clarification is needed.
- All state changes flow through the shared application service layer; security logic is never duplicated across handlers or agent-specific code paths.
- State changes are persisted before being reported as successful.

## Validation before submitting

```sh
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## Commits

Write clear, imperative commit messages. Include the rationale for security-relevant changes.
