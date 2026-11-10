# Notifications

Status: implemented.

Notifications surface system and agent events to the user, including pending
approval requests. Each one has a title, a body, a severity, and optional
action buttons.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/notifications` | List notifications |
| GET | `/api/v1/notifications/unread-count` | Unread count for the sidebar badge |
| GET | `/api/v1/notifications/{notification_id}` | Retrieve a notification |
| POST | `/api/v1/notifications/{notification_id}/read` | Mark a notification as read |
| POST | `/api/v1/notifications/read-all` | Mark all notifications as read |
| POST | `/api/v1/notifications/{notification_id}/actions/{action_id}` | Choose an action offered by the notification |
| DELETE | `/api/v1/notifications/{notification_id}` | Delete a notification |

## Representation

| Field | Meaning |
|---|---|
| `severity` | `info`, `success`, `warning`, or `error` |
| `actions` | Buttons: `{ id, label, style, kind, payload }` |
| `action_id` | The action the user chose, or `null` |
| `acted_at` | When the action was chosen, or `null` |

## Action kinds

| Kind | Behaviour |
|---|---|
| `agent` | The payload's `prompt` is handed to the assistant, which acts on the notification in a conversation |
| `dismiss` | The notification is closed without further work |

The agent creates notifications with actions through the `notify_user` tool,
which accepts `title`, `body`, `severity`, and up to four
`{ label, prompt, style }` actions.

## Rules

- Approval decisions use the [approval endpoints](approvals.md), never a notification's read state.
- Reading or dismissing a notification never authorizes an operation. An `agent`
  action is a request, not an authorization: privileged tools still require
  approval.
- Choosing an action marks the notification read and records `action_id`.
- Unknown action identifiers return 404 rather than being silently ignored.
- Notifications are delivered in real time over the [WebSocket](websocket.md) event stream (`notification.created`, `notification.acted`).
- Notifications backed by provider quota limits are created automatically by
  the rate-limit service (see [rate limits](../architecture/rate-limits.md)).