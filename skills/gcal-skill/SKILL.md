# Google Calendar Skill

Read and manage events on the connected Google Calendar.

## When to use

- The user asks about upcoming meetings or scheduling conflicts.
- The user asks to schedule, move, or cancel events.
- The user asks for scheduled reminders or calendar monitoring (for example, "check my calendar every morning and notify me if I have more than three meetings").

## Tools

| Tool | Risk | Behavior |
|---|---|---|
| `gcal_list_events` | low | List upcoming events |
| `gcal_search_events` | low | Search events by text |
| `gcal_create_event` | medium | Create an event |
| `gcal_update_event` | medium | Update an event |
| `gcal_cancel_event` | high | Cancel an event; **requires approval** |

## Rules

- When checking for conflicts, compare overlapping time ranges and report them clearly.
- Monitoring tasks notify the user only when the requested condition is met, unless daily summaries were requested.
- Cancellation is destructive: always confirm the exact event with the user and require approval.

## Requirements

A connected Google account with the `calendar.events` scope. Connect it in the Integrations view.