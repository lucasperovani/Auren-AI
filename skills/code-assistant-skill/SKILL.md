# Code Assistant Skill

Read, create, and edit code in the workspace — Copilot-style.

## When to use

- The user asks the agent to write, fix, refactor, or explain code.
- The user asks to explore or search a codebase in the workspace.

## Tools

| Tool | Risk | Behavior |
|---|---|---|
| `code_list_dir` | low | Explore the project tree |
| `code_read_file` | low | Read a file with line numbers |
| `code_search` | low | Search code by text |
| `code_write_file` | medium | Create or overwrite a file |
| `code_edit_block` | medium | Surgical find/replace edit |

## Workflow (Copilot-style)

1. **Explore first**: `code_list_dir` to see the structure, `code_search` to find relevant code.
2. **Read before editing**: always `code_read_file` the target region before changing it.
3. **Edit surgically**: prefer `code_edit_block` with the exact original text over rewriting whole files. The edit is rejected unless `old_text` matches exactly one location — include enough surrounding lines to make it unique.
4. **Verify**: re-read the edited region after the change.

## Rules

- Nothing is executed automatically: running code, tests, or shell commands is a separate, explicitly permissioned capability that this skill does not provide.
- All edits stay inside the workspace; path traversal is rejected.
- When creating a new file, prefer the smallest complete implementation that satisfies the request.