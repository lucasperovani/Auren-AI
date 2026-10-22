# WebSocket API

Endpoint: `/api/v1/events` (dedicated, authenticated with the application's session mechanism — never a reusable token in the URL).

## Authentication

The connection is authenticated with a **single-use, short-lived ticket** obtained over an authenticated REST call:

1. `POST /api/v1/events/ticket` with the session token in the body:

   ```json
   { "session_token": "current-session-token" }
   ```

   Response (`200 OK`):

   ```json
   { "ticket": "one-time-ticket", "expires_at": "2026-10-22T12:00:30Z" }
   ```

2. Open the WebSocket with the ticket as a query parameter:

   ```
   GET /api/v1/events?ticket=<one-time-ticket>
   ```

The ticket is valid for 30 seconds, is bound to the authenticated user, and is consumed atomically on first use. Unknown, expired, or already-used tickets are rejected with `401 Unauthorized` before the upgrade completes. No reusable session token ever appears in a URL or query string.

## Event envelope

Every event includes an identifier, timestamp, event type, and relevant resource identifiers:

```json
{
  "event_id": "event-identifier",
  "event_type": "task.started",
  "timestamp": "2026-10-22T12:00:00Z",
  "data": {
    "task_id": "task-identifier",
    "task_run_id": "run-identifier"
  }
}
```

## Event types

| Type | Emitted when |
|---|---|
| `task.created` | A task is created |
| `task.updated` | Task metadata, schedule, or status changes |
| `task.started` | A run starts executing |
| `task.completed` | A run completes successfully |
| `task.failed` | A run fails |
| `task.paused` | A task or run is paused |
| `task.cancelled` | A task or run is cancelled |
| `task_run.step_started` | A step begins |
| `task_run.step_completed` | A step finishes |
| `task_run.approval_required` | A step awaits approval |
| `approval.updated` | An approval request changes state |
| `notification.created` | A notification is created |
| `skill.upload_started` | A skill upload begins processing |
| `skill.validation_completed` | Package validation finishes |
| `skill.installed` | Installation and registration succeed |
| `skill.installation_failed` | Installation fails |
| `integration.status_changed` | An integration or account changes state |

## Constraints

- No credentials, private OAuth data, or unrestricted tool outputs are transmitted in broadcast events.
- Important events are persisted where appropriate; the WebSocket is a live-update channel, **not** the source of truth for task state.
- Clients recover missed events through REST collection endpoints (for example `GET /api/v1/task-runs/{run_id}/events`) or a documented event cursor.
- The channel is one-way (server → client). The server sends protocol-level pings every 30 seconds; clients that send non-control frames are disconnected.

## Reconnection

Clients should reconnect with exponential backoff: request a new ticket with `POST /api/v1/events/ticket`, reconnect, and re-fetch current state from REST; the server does not replay events across connections.