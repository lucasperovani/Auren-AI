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

## Rules

- The upload endpoint accepts `multipart/form-data`.
- The maximum upload size is configurable, with a safe default; oversized uploads return `413`.
- Installation is asynchronous when validation or extraction could take significant time: the upload returns an `upload_id`, and progress is exposed through the status endpoint and [WebSocket](websocket.md) events.
- The uploaded ZIP is never directly executable; it is extracted and validated in a controlled location.
- Package format and installation lifecycle details: [package format](../skills/package-format.md), [installation lifecycle](../skills/installation-lifecycle.md).