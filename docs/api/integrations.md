# Integrations

Status: Contract defined; not yet implemented.

Integrations connect external providers (for example email or calendar services) through OAuth-based connection flows.

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/integrations` | List integration providers and configured connections |
| GET | `/api/v1/integrations/catalog` | List available integration types |
| POST | `/api/v1/integrations/{provider_type}/connect` | Initiate a connection flow |
| GET | `/api/v1/integrations/oauth/callback` | Process the validated OAuth callback |
| GET | `/api/v1/integrations/{integration_id}` | Retrieve integration details |
| DELETE | `/api/v1/integrations/{integration_id}` | Disconnect an integration |
| GET | `/api/v1/integrations/{integration_id}/accounts` | List connected accounts |
| GET | `/api/v1/integrations/{integration_id}/accounts/{account_id}` | Retrieve account status and granted scopes |
| POST | `/api/v1/integrations/{integration_id}/accounts/{account_id}/test` | Test the connection |
| POST | `/api/v1/integrations/{integration_id}/accounts/{account_id}/reconnect` | Reauthorize a connection |
| GET | `/api/v1/integrations/{integration_id}/accounts/{account_id}/capabilities` | List supported operations |

## Rules

- The OAuth callback validates the original authorization request, including `state` and the expected provider.
- Credential values are never exposed in any integration response.
- The capability list reflects implemented operations and the account's current permissions, not the theoretical capabilities of the provider's API.
- The OAuth callback is the only integration endpoint reachable without a session token (see [authentication](authentication.md)).