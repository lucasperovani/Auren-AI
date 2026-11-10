# Skills

Status: Contract defined; not yet implemented.

Skills are installable capability packages. Installation happens entirely through the API and frontend; no shell access or manual filesystem operations are required.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/skills` | List installed skills |
| POST | `/api/v1/skills/uploads` | Upload a skill ZIP for validation and installation |
| GET | `/api/v1/skills/uploads/{upload_id}` | Retrieve upload and installation status |
| GET | `/api/v1/skills/{skill_id}` | Retrieve skill metadata and current version |
| PATCH | `/api/v1/skills/{skill_id}` | Update permitted skill settings |
| DELETE | `/api/v1/skills/{skill_id}` | Uninstall a skill |
| POST | `/api/v1/skills/{skill_id}/enable` | Enable a skill |
| POST | `/api/v1/skills/{skill_id}/disable` | Disable a skill |
| GET | `/api/v1/skills/{skill_id}/versions` | List installed versions |
| POST | `/api/v1/skills/{skill_id}/versions/{version_id}/activate` | Activate a validated version |
| POST | `/api/v1/skills/{skill_id}/validate` | Revalidate the installed package and capabilities |
| GET | `/api/v1/skills/{skill_id}/permissions` | Inspect declared permissions |
| PUT | `/api/v1/skills/{skill_id}/permissions` | Update configurable permissions |
| GET | `/api/v1/skills/{skill_id}/tools` | List the tools declared by the active version |
| POST | `/api/v1/skills/import/discover` | List skills, tools, and MCP servers a GitHub repository provides |
| POST | `/api/v1/skills/import/install` | Download and install a skill package discovered in a repository |

## Importing from a repository

`POST /skills/import/discover` takes `{ repository, integration_account_id? }`
where `repository` is `owner/repo`, a GitHub URL, or a URL with
`/tree/<ref>/<path>`. It returns the discovered items and the resolved
reference:

| Field | Contents |
|---|---|
| `repository` / `reference` | Normalized repository and the commit, branch, or tag scanned |
| `skills` | Directories containing a `skill.json` (`path`, `id`, `name`, `version`) |
| `tools` | JSON tool definitions found under `tools/` |
| `mcp_servers` | `.mcp.json`/`mcp.json` entries with their `transport` |
| `notes` | Skipped or unreadable files |

Discovery never installs anything. `POST /skills/import/install` takes
`{ repository, reference, skill_path, install }` and returns an `upload_id`
that follows the regular upload status protocol, so a repository package goes
through the same manifest, schema, and capability validation as an uploaded ZIP.

Only `http` MCP servers can be connected from the UI; `stdio` servers would
require launching a local process and are reported as unsupported.

Private repositories use the user's connected GitHub account. Without one, only
public repositories are readable.

## Rules

- The upload endpoint accepts `multipart/form-data`.
- The maximum upload size is configurable, with a safe default; oversized uploads return `413`.
- Installation is asynchronous when validation or extraction could take significant time: the upload returns an `upload_id`, and progress is exposed through the status endpoint and [WebSocket](websocket.md) events.
- The uploaded ZIP is never directly executable; it is extracted and validated in a controlled location.
- Package format and installation lifecycle details: [package format](../skills/package-format.md), [installation lifecycle](../skills/installation-lifecycle.md).