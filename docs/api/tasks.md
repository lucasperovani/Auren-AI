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
| GET | `/api/v1/tasks/{task_id}/runs` | List execution history |
| GET | `/api/v1/tasks/{task_id}/schedule` | Retrieve scheduling information |
| PUT | `/api/v1/tasks/{task_id}/schedule` | Update scheduling information |
| GET | `/api/v1/tasks/{task_id}/steps` | Inspect the latest or selected execution steps |

Filters support status, agent, creation time, and schedule state.

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