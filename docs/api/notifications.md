# Notifications

Status: Contract defined; not yet implemented.

Notifications surface system and agent events to the user, including pending approval requests.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/notifications` | List notifications |
| GET | `/api/v1/notifications/{notification_id}` | Retrieve a notification |
| POST | `/api/v1/notifications/{notification_id}/read` | Mark a notification as read |
| POST | `/api/v1/notifications/read-all` | Mark all notifications as read |
| DELETE | `/api/v1/notifications/{notification_id}` | Delete a notification when permitted |

## Rules

- Approval decisions use the [approval endpoints](approvals.md), never a notification's read state.
- Reading or dismissing a notification never authorizes an operation.
- Notifications are delivered in real time over the [WebSocket](websocket.md) event stream; this API provides retrieval and state management.