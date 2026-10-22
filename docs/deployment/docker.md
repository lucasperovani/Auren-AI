# Docker

The server is a single static Rust binary packaged in a minimal container image.

## Image

```dockerfile
FROM rust:1.85 AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p auren-server

FROM debian:bookworm-slim
COPY --from=build /src/target/release/auren-server /usr/local/bin/auren-server
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/auren-server"]
```

## Data volume

Mount a persistent volume at `/data`. Everything that must survive restarts lives there:

```text
/data/
├── auren.db      # SQLite database (+ WAL/SHM sidecars)
├── skills/       # installed skill packages
├── uploads/      # temporary upload staging
└── workspace/    # default authorized workspace root
```

## Running

```sh
docker run -d \
  --name auren \
  -p 8080:8080 \
  -v auren-data:/data \
  -e AUREN_DATABASE_URL=sqlite:///data/auren.db?mode=rwc \
  -e AUREN_SKILLS_DIR=/data/skills \
  -e AUREN_SESSION_SECRET=<random-secret> \
  auren-ai
```

## docker-compose.yml

```yaml
services:
  auren:
    build: .
    ports:
      - "8080:8080"
    volumes:
      - auren-data:/data
    environment:
      AUREN_DATABASE_URL: sqlite:///data/auren.db?mode=rwc
      AUREN_SKILLS_DIR: /data/skills
      AUREN_SESSION_SECRET: <random-secret>
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://127.0.0.1:8080/health/live"]
      interval: 30s
      timeout: 5s
      retries: 3
volumes:
  auren-data:
```

## Health checks

| Endpoint | Meaning |
|---|---|
| `GET /health/live` | Process is up and serving requests |
| `GET /health/ready` | Database is reachable and migrations are applied |

Use `/health/live` for container liveness probes and `/health/ready` for load-balancer readiness.

## Notes

- Changing host mount configuration may require a container restart (see [Workspace](../architecture/workspace.md)).
- A container alone is not a sufficient reason to trust uploaded executable code; the first release supports declarative skills only.
- See [Configuration](configuration.md) for all environment variables.
