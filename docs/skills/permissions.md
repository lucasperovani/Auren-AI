# Skill Permissions

## Principle

Installing or enabling a skill never authorizes its tools by itself. Every tool invocation passes through the single permission-engine choke point, which evaluates:

1. **Agent-level permissions** (`agent_tool_permissions`): `allow`, `deny`, or `ask` per tool. Agent configuration can restrict but never override mandatory security restrictions.
2. **Global security policy**: mandatory rules outside agent control (for example, critical-risk tools always require approval).
3. **Skill availability**: a disabled or uninstalled skill cannot start new invocations. For running tasks, an explicit policy applies: already-authorized safe steps may finish or the task is cancelled — a disabled skill never silently initiates new operations.
4. **Integration account and scopes**: the operation must be covered by the connected account's granted OAuth scopes.
5. **Operation arguments**: validated against the tool's declared input schema.
6. **Approval requirements**: operations with `risk_class` `high` or `critical` (or policy `ask`) create an approval request bound to the canonical operation payload and its hash. Any material change to the operation invalidates the approval and requires a new one.

## Declaration vs. grant

The manifest may *request* capabilities (`required_permissions`), but the manifest cannot grant itself permissions. Requests are validated against the application's security policy during installation (stage 5 of the pipeline), and effective authorization is decided at invocation time.

## Risk classes

| Class | Default behavior |
|---|---|
| `low` | Allowed when the agent has an `allow` policy |
| `medium` | Allowed with `allow`; logged |
| `high` | Requires explicit approval unless policy explicitly allows |
| `critical` | Always requires explicit approval |

## Approvals

Approval requests are authenticated, time-limited, and bound to the exact operation (canonical payload + hash). The server revalidates the operation before execution. A changed operation requires a new approval. Approval decisions are made through the approval API endpoints — reading or dismissing a notification never authorizes an operation.