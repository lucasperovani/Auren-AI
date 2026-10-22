# Security Model

## Trust boundaries

| Boundary | Inside (trusted) | Outside (untrusted) |
|---|---|---|
| Process | Axum server, service layer, tool registry | Uploaded skill packages, external LLM providers, external APIs |
| Filesystem | Application code, database, credential keys, installed skill directories | Upload staging area, user-provided workspace files |
| Network | Localhost admin surface | All outbound calls (providers, integrations) |

## Authorization model

Authorization is evaluated at a single choke point — the **permission engine** — whenever a tool is invoked. It considers, in order:

1. **Authentication**: a valid, unexpired session (or an internal system actor).
2. **Ownership**: the requesting user must own the resource; knowing an identifier grants nothing.
3. **Agent-level permissions** (`agent_tool_permissions`): `allow`, `deny`, or `ask` per tool. Agent configuration can restrict but never override mandatory security restrictions.
4. **Skill availability**: a disabled or uninstalled skill's tools cannot start new invocations.
5. **Integration account and scopes**: the operation must be covered by the account's granted OAuth scopes.
6. **Operation arguments**: validated against the tool's input schema.
7. **Approval requirements**: high-risk operations require an explicit, time-limited approval bound to the exact operation (canonical payload + hash). Any material change to the operation invalidates the approval.

## Secrets handling

- Passwords: Argon2id hashes only.
- Sessions: only a keyed hash of the session token is stored; plaintext tokens are never persisted.
- API keys and OAuth tokens: authenticated encryption (ciphertext + nonce + key version); the encryption key is kept separate from the encrypted data.
- Logs and audit events never contain passwords, API keys, OAuth tokens, or session tokens.
- API responses never return stored secrets.

## Input handling

- Uploaded ZIP archives are treated as untrusted input: validated before extraction, with path traversal, absolute paths, symlinks, duplicate entries, size and compression-ratio limits enforced.
- File operations resolve against explicitly authorized roots only; traversal outside a root is rejected.
- Provider URLs are validated to prevent requests to unauthorized internal destinations.
- Error responses never expose stack traces, SQL, or provider internals.

## Audit

Audit events are append-only at the application level and record actor, event type, and non-sensitive details for every security-relevant action.