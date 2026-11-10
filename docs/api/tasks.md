# Tasks

Status: Contract defined; not yet implemented.

Tasks are persistent, schedulable units of work. A task run is one execution of a task. A task plan is a proposed task generated from natural language, which becomes a task only after validation and confirmation.

## Tasks

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/tasks` | List tasks with pagination and filters |
| POST | `/api/v1/tasks` | Create a structured task |
| GET | `/api/v1/tasks/{task_id}` | Retrieve task details |
| PATCH | `/api/v1/tasks/{task_id}` | Update instructions, conditions, or schedule |
| DELETE | `/api/v1/tasks/{task_id}` | Cancel and delete or archive a task |
| POST | `/api/v1/tasks/{task_id}/run` | Request immediate execution |
| POST | `/api/v1/tasks/{task_id}/pause` | Pause future executions |
| POST | `/api/v1/tasks/{task_id}/resume` | Resume future executions |
| POST | `/api/v1/tasks/{task_id}/cancel` | Cancel active work where possible |
| POST | `/api/v1/tasks/{task_id}/stage` | Move a task to another board column |
| POST | `/api/v1/tasks/{task_id}/assign` | Assign the task to the agent and start a run |
| GET | `/api/v1/tasks/{task_id}/notes` | Notes on the card |
| POST | `/api/v1/tasks/{task_id}/notes` | Add a note |
| DELETE | `/api/v1/tasks/{task_id}/notes/{note_id}` | Delete a note |
| GET | `/api/v1/tasks/{task_id}/links` | References to and from the card |
| POST | `/api/v1/tasks/{task_id}/links` | Link another task |
| DELETE | `/api/v1/tasks/{task_id}/links/{link_id}` | Remove a link |
| GET | `/api/v1/tasks/{task_id}/activity` | Chronological timeline of notes, runs, and tool calls |
| GET | `/api/v1/tasks/{task_id}/runs` | List execution history |
| GET | `/api/v1/tasks/{task_id}/schedule` | Retrieve scheduling information |
| PUT | `/api/v1/tasks/{task_id}/schedule` | Update scheduling information |
| GET | `/api/v1/tasks/{task_id}/steps` | Inspect the latest or selected execution steps |

Filters support status, agent, creation time, and schedule state.

### Task representation

List and detail responses describe the schedule without exposing cron syntax
as the user-facing label:

| Field | Meaning |
|---|---|
| `schedule` | Raw schedule object, e.g. `{ "cron": "0 9 * * 1-5" }`, or `null` |
| `schedule_cron` | The cron expression alone, for clients that render their own description |
| `next_run_at` | Next fire time (RFC 3339) while the schedule is active |
| `last_run_status` | Status of the most recent run, or `null` |
| `last_run_at` | Creation time of the most recent run, or `null` |
| `stage` | Kanban column: `analysis`, `todo`, `in_progress`, or `done` |

`PATCH /tasks/{id}` treats `schedule` as follows: omitted leaves the schedule
unchanged, `null` removes the schedule and returns the task to on-demand
(`draft`), and an object replaces it and reactivates the task.

### Kanban stage

`stage` is the board position and is **independent of `status`**: `status` is
the execution state owned by the scheduler (it decides whether a schedule
fires), while `stage` only tracks where a card sits.

| Stage | Meaning |
|---|---|
| `analysis` | Still needs scope or requirements |
| `todo` | Ready to run |
| `in_progress` | Actively being worked |
| `done` | Closed |

Movement:

- `POST /tasks/{task_id}/stage` with `{ "stage": "in_progress" }` moves a card.
  `PATCH /tasks/{task_id}` also accepts `stage`.
- New tasks start in `todo` when created with a schedule and in `analysis`
  otherwise. Removing a schedule returns the task to `analysis`.
- A run starting moves the card to `in_progress`; a run completing moves it to
  `done`. Cancelling or completing a task also closes the card.
- Dragging never changes `status`: a scheduled task keeps firing wherever it
  sits, so moving a card cannot silently pause work.
- Unknown stages return `422`; reopening a closed task (`completed`/`cancelled`)
  returns `409`.

### Assigning work

`POST /tasks/{task_id}/assign` is the **only** stage change that starts work: it
moves the card to `in_progress` and queues a run. Dragging a card never runs
anything, so an accidental drop costs nothing.

- The UI asks for confirmation when a card lands in To Do; dismissing the
  dialog leaves it there without running.
- Assignment fails with `409` when a `blocks` link points at unfinished work,
  naming the blocking card. The card stays where it was.
- The agent uses `task_assign` for the same operation when it decides to do the
  work itself or delegate it to a parallel run.

### Projects

Tasks belong to an optional project (`project_id`, `project_name`). Boards,
listings, and the agent's `task_list` are scoped by project, so work on one
project does not interfere with another. Tasks without a project live on the
default board and are selected with `?project_id=none`.

Deleting a project detaches its tasks (`project_id = NULL`) instead of deleting
them.

### Notes and links

- Notes (`author` is `user` or `agent`) hold the running context of a card,
  separate from the objective, which stays the stable instruction.
- Links point from one task to another with `kind` `blocks` or `relates`.
  `A blocks B` means A must finish before B; `GET /links` returns `outgoing`
  and `incoming`, so both sides render. Self-links and re-linking the same pair
  are handled explicitly (rejected and idempotent respectively).
- `GET /activity` returns notes, runs, and tool steps in one chronological
  list, which is what the card detail shows under "What the agent did".

## Task runs

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/task-runs/{run_id}` | Retrieve execution status and results |
| POST | `/api/v1/task-runs/{run_id}/pause` | Pause an execution when safe |
| POST | `/api/v1/task-runs/{run_id}/resume` | Resume a paused execution |
| POST | `/api/v1/task-runs/{run_id}/cancel` | Request cancellation |
| GET | `/api/v1/task-runs/{run_id}/steps` | Retrieve execution steps |
| GET | `/api/v1/task-runs/{run_id}/events` | Retrieve persisted execution events |

## Task plans

| Method | Endpoint | Purpose |
|---|---|---|
| POST | `/api/v1/task-plans` | Interpret a natural-language request and generate a proposed plan |
| GET | `/api/v1/task-plans/{plan_id}` | Retrieve the proposed plan and its validation state |
| PATCH | `/api/v1/task-plans/{plan_id}` | Refine the plan with additional information |
| POST | `/api/v1/task-plans/{plan_id}/clarifications` | Submit answers to clarification questions |
| POST | `/api/v1/task-plans/{plan_id}/confirm` | Validate and create the persistent task |
| POST | `/api/v1/task-plans/{plan_id}/cancel` | Discard the proposed plan |

## Rules

- Ownership is enforced on every task, run, and plan lookup.
- Collections use cursor pagination (see [conventions](README.md)).
- Cancellation is cooperative: already completed external operations are not reversed.
- Uncertain outcomes are exposed as such, never reported as false success or failure.
- A task plan is not an active task until it passes validation and is confirmed.
- Plan confirmation creates the task and its schedule transactionally.
- A plan identifies missing information, required skills, integrations, permissions, and approval requirements; the agent asks the user rather than inventing essential details.
- Tasks can also be created and updated by the conversational agent through internal service methods that enforce the same business rules as the REST API.