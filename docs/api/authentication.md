# Authentication

## Session mechanism

Auren-AI uses bearer session tokens. `POST /api/v1/auth/login` returns a session token once; only a keyed SHA-256 hash of the token is stored server-side. Tokens expire after 30 days.

All application endpoints require authentication except:

- `GET /health/live` and `GET /health/ready`
- `GET /api/v1/auth/setup-status`
- `GET /api/v1/integrations/oauth/callback` (validated against the original authorization request)

## Endpoints

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/api/v1/auth/setup-status` | Whether initial administrator setup is required |
| POST | `/api/v1/auth/setup` | Create the initial administrator (refused once any user exists) |
| POST | `/api/v1/auth/login` | Authenticate and issue a session |
| POST | `/api/v1/auth/logout` | Revoke the current session |
| GET | `/api/v1/auth/me` | Retrieve the current user |
| POST | `/api/v1/auth/change-password` | Change password and revoke other sessions |

## Rules

- No default administrator password exists; setup is only possible while no user exists.
- Passwords are hashed with Argon2id.
- Login responses do not reveal whether an email exists.
- Changing a password revokes all existing sessions.
- CSRF protection applies to cookie-based flows; the current token-based flow is not cookie-authenticated.

## Example

```sh
curl -X POST http://localhost:8080/api/v1/auth/login \
  -H 'content-type: application/json' \
  -d '{"email": "admin@example.com", "password": "..."}'
```

Subsequent requests carry the token:

```sh
curl http://localhost:8080/api/v1/auth/me \
  -H 'authorization: Bearer <session_token>'
```