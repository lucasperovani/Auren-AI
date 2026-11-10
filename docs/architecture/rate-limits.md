# Rate Limits and Deferred Operations

Status: implemented.

Providers publish hard quotas (Instagram, for example, allows a limited number
of API-published posts per 24-hour moving period). Auren never polls a provider
waiting for capacity: when a quota is exhausted the operation is persisted,
the user is notified, and a background worker runs it once the window reopens.

## Model

| Table | Purpose |
|---|---|
| `rate_limit_usage` | Rolling-window counters per integration account and bucket |
| `deferred_operations` | Operations queued behind a quota, with retry state |

A `bucket` is a logical operation family, not a provider endpoint:

| Provider | Bucket | Window | Limit |
|---|---|---|---|
| Instagram | `publish` | 24 h | 50 posts |
| Instagram | `messages` | 1 h | 200 |
| Instagram | `reads` | 1 h | 400 |

Limits are conservative on purpose: a personal assistant should degrade
gracefully well before the platform throttles the app. Instagram additionally
exposes `content_publishing_limit`, which is authoritative and can be read with
the `get_publishing_limit` operation.

## Flow

1. Before a metered call, the quota is checked. If it is already exhausted the
   call is **not** attempted.
2. The operation is stored in `deferred_operations` with `available_at` set to
   the window reset plus a 30-second margin. Repeating the same operation
   payload refreshes the existing entry instead of queueing a duplicate.
3. A notification explains the wait and offers a "Run now" action that asks the
   agent to run it immediately.
4. The worker (`services::worker::tick_once`) picks up due operations once per
   tick, executes them through the normal integration path, and records usage.
5. On success the entry completes and the user is told. On a transient failure
   the entry is retried with capped exponential backoff (30 s, 1 m, 2 m, 4 m… up
   to 1 h). After five attempts it is marked failed and reported with an action
   to have the agent investigate.

If the provider rejects a call with `429` even though local accounting said
there was room, the operation is deferred for the next window rather than
reported as a failure.

## Anti-polling rules

- No tight loops: waits are scheduled, not slept on inside a request.
- No LLM involvement in waiting: the agent is only asked to act when the user
  chooses an action on a notification.
- Bounded container-status polling for Instagram publishing (about five
  attempts with exponential delays), which is the documented flow.
- Deferred work for a disconnected account is cancelled so a revoked token
  cannot generate retries.

## Observability

`GET /api/v1/integrations/{integration_id}/accounts/{account_id}/rate-limits`
returns the current window usage per bucket plus the pending deferred
operations, so the UI can show remaining capacity and explain the queue.
