# Automation Skill

Create and manage scheduled tasks, execute commands and Python programs, connect to remote servers over SSH, and send messages to the user.

## When to use

- The user asks for recurring or scheduled work ("every day at 16:40, create a.txt with banana").
- The user asks to run a command, script, or Python program.
- The user asks to connect to a remote server (SSH) and run commands there.
- The user asks to pause, resume, modify, or delete an existing task.
- The agent needs to report a result to the user in the interface.

## Tools

| Tool | Risk | Behavior |
|---|---|---|
| `task_create` | medium | Create a task; with `schedule.cron` it runs automatically |
| `task_list` | low | List tasks |
| `task_update` | medium | Update title/objective/schedule |
| `task_delete` | high | Permanently delete; **requires approval** |
| `task_pause` / `task_resume` | low | Pause/resume future executions |
| `task_run` | medium | Run immediately |
| `exec_shell` | high | Shell command in the workspace; **requires approval** |
| `exec_python` | high | Python in the workspace; **requires approval** |
| `ssh_connect` | high | Register a remote server connection; **requires approval** |
| `ssh_list` | low | List registered SSH connections |
| `exec_ssh` | high | Run a command on a registered remote server; **requires approval** |
| `ssh_disconnect` | high | Remove a registered connection; **requires approval** |
| `notify_user` | low | Message to the user in the interface |
| `subagent_create` | low | Create a specialized subagent (inherits your model) |
| `subagent_delegate` | medium | Delegate an objective; returns the subagent's result |
| `subagent_list` | low | List available agents |
| `subagent_delete` | medium | Delete a subagent that is no longer needed |

## Scheduling rules

- The user speaks in their local timezone; convert to UTC for the cron expression and say so in your confirmation. For example, "todos os dias às 16:40" in America/Sao_Paulo (UTC-3) is `40 19 * * *` in UTC.
- Cron format: `minute hour day-of-month month day-of-week` (5 fields, UTC).
- After creating a scheduled task, confirm clearly: what will run, when (in the user's timezone), and what it will do.
- When the user asks to modify an existing task, use `task_list` to find it first; never create a duplicate.
- When a task's objective involves files or commands, write the objective so a future run can execute it: state the exact file, content, or command.

## Execution rules

- Shell and Python run inside the workspace with a 60-second timeout.
- Destructive commands (`rm -rf /`, disk writes, shutdown) are rejected outright.
- Both tools require approval by default; tell the user to approve in the Approvals view when needed.

## SSH rules

- Workflow: `ssh_connect` once per server (get a `connection_id`), then `exec_ssh` with that id for each command; `ssh_list` shows existing connections — reuse one instead of reconnecting.
- Authentication uses the server host's key-based SSH setup (agent, default key files, or ssh_config entries); passwords are never stored or prompted.
- Commands run with a 60-second timeout and the same safety policy as `exec_shell` (destructive commands are rejected).
- `exec_ssh` requires approval by default; tell the user to approve in the Approvals view when needed.
- Remove connections that are no longer needed with `ssh_disconnect`.

## Subagents

- Create a subagent when a focused helper helps (research, analysis, calculation). It inherits your LLM provider and model.
- `subagent_delegate` returns the subagent's final answer — relay it to the user.
- Clean up: delete one-off subagents after their work is done (`subagent_delete`), unless you expect to reuse them.

## Reporting

- Use `notify_user` to deliver results the user should see later (task outcomes, findings, monitoring alerts).
- In chat, summarize what you did and show the relevant output.