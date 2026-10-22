# Multi-stage build: compile the Rust server, then assemble a minimal
# runtime image with the binary, frontend assets, and migrations.
FROM rust:1.85 AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release -p auren-server

FROM debian:bookworm-slim AS runtime
# CA certificates for outbound HTTPS (LLM providers, OAuth, web research).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user (spec section 15).
RUN useradd --system --create-home --home-dir /app --shell /usr/sbin/nologin auren
WORKDIR /app

COPY --from=builder /build/target/release/auren-server /app/auren-server
COPY --from=builder /build/migrations /app/migrations
COPY frontend /app/frontend

# Persistent directories: application data, skills, and the agent workspace.
RUN mkdir -p /data /data/skills /data/workspace /workspace \
    && chown -R auren:auren /app /data /workspace

USER auren

ENV AUREN_DATABASE_URL=sqlite:///data/auren.db?mode=rwc \
    AUREN_BIND_ADDR=0.0.0.0:8080 \
    AUREN_SKILLS_DIR=/data/skills \
    AI_WORKSPACE_CONTAINER_PATH=/workspace

EXPOSE 8080

# Container health check (spec section 18).
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health/ready || exit 1

ENTRYPOINT ["/app/auren-server"]