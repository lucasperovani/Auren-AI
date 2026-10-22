# Approvals

Status: Contract defined; not yet implemented.

Approvals gate sensitive operations behind explicit, auditable user decisions.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/approvals` | List pending and historical approvals |
| GET | `/api/v1/approvals/{approval_id}` | Retrieve the complete operation details |
| POST | `/api/v1/approvals/{approval_id}/approve` | Approve a specific operation |
| POST | `/api/v1/approvals/{approval_id}/reject` | Reject a specific operation |
| POST | `/api/v1/approvals/{approval_id}/cancel` | Cancel a pending approval request when permitted |

## Rules

- Approvals are authenticated, time-limited, and bound to the exact operation.
- The operation is bound by a canonical payload and its hash; the server revalidates the operation against that binding before execution.
- A changed operation requires a new approval.
- Reading a notification never authorizes an operation; decisions happen only through these endpoints (see [notifications](notifications.md)).
- Approval details expose the full operation so the user can make an informed decision.

## Example

```sh
curl -X POST http://localhost:8080/api/v1/approvals/{approval_id}/approve \
  -H 'authorization: ******'
```