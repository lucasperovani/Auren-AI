# Agents

Status: Contract defined; not yet implemented.

Agents are persistent assistant configurations with their own instructions, provider/model selection, skills, and permissions.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/agents` | List agents |
| POST | `/api/v1/agents` | Create an agent |
| GET | `/api/v1/agents/{agent_id}` | Retrieve agent details |
| PATCH | `/api/v1/agents/{agent_id}` | Update instructions, model, and settings |
| DELETE | `/api/v1/agents/{agent_id}` | Delete or archive an agent according to retention rules |
| POST | `/api/v1/agents/{agent_id}/pause` | Pause an agent's future execution |
| POST | `/api/v1/agents/{agent_id}/resume` | Resume an agent |
| GET | `/api/v1/agents/{agent_id}/skills` | List skills available to the agent |
| PUT | `/api/v1/agents/{agent_id}/skills/{skill_id}` | Enable or disable a skill for an agent |
| GET | `/api/v1/agents/{agent_id}/permissions` | Retrieve agent-level permissions |
| PUT | `/api/v1/agents/{agent_id}/permissions` | Update configurable permissions |

## Rules

- Agent-level configuration cannot override mandatory security restrictions.
- Each agent selects its LLM provider and model independently.
- Model capabilities are stored explicitly, not inferred from the model name.
- Deleting an agent follows the retention rules; it may archive rather than remove.
- Pausing affects future executions only; running work is governed by the [task run](tasks.md) semantics.

## Example

```sh
curl -X POST http://localhost:8080/api/v1/agents \
  -H 'authorization: ******' -H 'content-type: application/json' \
  -d '{"name": "Researcher", "instructions": "...", "provider": "openai", "model": "gpt-4o"}'
```