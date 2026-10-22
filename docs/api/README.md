# API

The REST API is versioned under `/api/v1`. The authoritative contract is [openapi.yaml](openapi.yaml).

## Contents

- [OpenAPI specification](openapi.yaml)
- [WebSocket events](websocket.md)
- [Authentication](authentication.md)

## Conventions

- JSON request and response bodies unless the endpoint handles a file upload or streaming.
- Consistent error format:

```json
{
  "error": {
    "code": "approval_required",
    "message": "This operation requires explicit approval.",
    "request_id": "request-identifier",
    "details": { "approval_id": "approval-identifier" }
  }
}
```

- Cursor-based pagination for large collections:

```json
{ "items": [], "next_cursor": null }
```

- Every resource lookup enforces ownership and authorization.
- No stack traces, secrets, SQL, or provider internals in responses.

## Status codes

| Code | Meaning |
|---|---|
| 200 | Successful read or update |
| 201 | Resource created |
| 202 | Asynchronous work accepted |
| 204 | Success without body |
| 400 | Malformed request |
| 401 | Authentication required |
| 403 | Insufficient permission |
| 404 | Missing or inaccessible resource |
| 409 | Invalid state transition or conflict |
| 413 | Upload too large |
| 422 | Valid JSON with invalid semantic content |
| 429 | Rate limit exceeded |
| 500 | Unexpected server failure |
| 503 | Required service unavailable |

## Endpoint groups

| Group | Spec section |
|---|---|
| Auth | 3.2 |
| LLM providers | 3.3 |
| Agents | 3.4 |
| Conversations | 3.5 |
| Tasks | 3.6 |
| Task runs | 3.7 |
| Task plans | 3.8 |
| Integrations | 3.9 |
| Skills | 3.10 |
| Approvals | 3.11 |
| Memory | 3.12 |
| Notifications | 3.13 |
| Workspace | 3.14 |
| Audit & health | 3.15 |
| Events (WebSocket) | 4 |