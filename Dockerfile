# Auren-AI — aarch64 image with the app-announcer runtime.
#
# Two stages:
#   1. builder  — compiles a static musl binary for aarch64.
#   2. runtime  — the app-announcer base image (arm64), which discovers the app
#                 over mDNS and supervises the server process.
#
# Build:
#   docker buildx build --platform linux/arm64 \
#     --build-arg TARGET=aarch64-unknown-linux-musl \
#     -t auren-ai:arm64 -f Dockerfile .
#
# Image size: the server binary is the only large artifact (~6 MB after UPX).
# Chromium, which `web_navigate` needs, is ~180 MB and is therefore *opt-in*:
# build with `--build-arg WITH_BROWSER=true` only when you need JavaScript
# rendering. Everything else (search, fetch, the whole app) works without it.
#
# `--platform=linux/arm64` pins the builder image architecture. Without it,
# `rust:1.97-alpine` resolves to the *runner's* architecture, so on an x86 host
# the toolchain would be amd64 while TARGET asks for aarch64 — producing either
# a cross-compile (slow, and openssl-src needs extra setup) or a broken build.
# Pinning makes the build native wherever it runs.
#
# The image version must satisfy the workspace MSRV (Cargo.toml
# `rust-version`), which the locked dependencies raise to 1.89.
FROM --platform=linux/arm64 rust:1.97-alpine AS builder
ARG TARGET=aarch64-unknown-linux-musl

# musl-dev/perl serve ring and openssl-src; git for crates.io git dependencies.
RUN apk add --no-cache musl-dev perl make git ca-certificates \
    && rustup target add ${TARGET}

WORKDIR /build

# ---------------------------------------------------------------------------
# Dependency cache
#
# Compiling the dependency graph takes several minutes and only changes when a
# manifest changes. Manifests are copied first and the crate is built against a
# stub source, so this layer is reused until Cargo.toml/Cargo.lock change — a
# code-only change then recompiles just this crate.
#
# A stub `main.rs` satisfies `cargo build -p auren-server` while the real
# sources are still absent; the dependencies get compiled and cached.
# ---------------------------------------------------------------------------
COPY Cargo.toml Cargo.lock ./
COPY crates/auren-server/Cargo.toml ./crates/auren-server/Cargo.toml

RUN mkdir -p crates/auren-server/src \
    && printf 'fn main() {}\n' > crates/auren-server/src/main.rs \
    && : > crates/auren-server/src/lib.rs \
    && RUSTFLAGS="-C target-feature=+crt-static" \
       cargo build --release --target ${TARGET} -p auren-server

# Remove the stub artifacts so the real build cannot reuse them. The dependency
# artifacts stay cached, which is the whole point.
RUN rm -rf crates/auren-server/src \
    target/${TARGET}/release/deps/auren_server* \
    target/${TARGET}/release/auren-server \
    target/${TARGET}/release/auren-server.d

# Real sources. Everything above is now cached.
COPY crates ./crates
# Migrations are embedded at compile time by sqlx::migrate!, so they must be
# present before the build, not after it.
COPY migrations ./migrations

# LTO + opt-level=z + panic=abort + symbol stripping come from the release
# profile in Cargo.toml, so the output is already the smallest codegen.
RUN RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target ${TARGET} -p auren-server

# ---------------------------------------------------------------------------
# Compression stage: UPX on a static musl binary typically halves it.
# Kept in its own stage so the UPX toolchain never reaches the runtime image.
#
# Pinned to arm64: UPX is an executable that rewrites the binary, so it must
# run on the architecture of the binary it compresses.
# ---------------------------------------------------------------------------
FROM --platform=linux/arm64 alpine:3.21 AS compress
ARG TARGET=aarch64-unknown-linux-musl
RUN apk add --no-cache upx
COPY --from=builder /build/target/${TARGET}/release/auren-server /auren-server
# --best is slow (seconds) but yields the smallest output; lzma is tighter than
# the default. `--force` is required for static binaries.
RUN upx --best --lzma --force /auren-server

# ---------------------------------------------------------------------------
# Runtime: app-announcer supervises the server and announces it on the LAN.
# The base image is arm64 only, which is why the whole build targets arm64.
# ---------------------------------------------------------------------------
FROM --platform=linux/arm64 registry.gitlab.com/lucas.perovani/app-announcer:latest
ARG TARGET=aarch64-unknown-linux-musl
ARG WITH_BROWSER=false

# Chromium is ~180 MB, more than everything else combined. It is installed only
# on request so the default image stays small.
#
# When it is absent the server detects that at startup and *hides* the tools
# that need it: the agent is never offered `web_navigate`, so a user cannot hit
# a silent failure. No environment variable is needed either way: the server
# probes PATH, and Alpine installs `/usr/bin/chromium-browser`.
RUN if [ "$WITH_BROWSER" = "true" ]; then \
        apk add --no-cache chromium ca-certificates; \
    else \
        echo "Chromium skipped (WITH_BROWSER=false); web_navigate will not be offered"; \
    fi

COPY --from=compress /auren-server /app/auren-server
# The SPA is served from disk, so the assets ship inside the image.
COPY frontend /static
# Built-in skill packages are read at runtime when a user installs one.
COPY skills /app/skills
COPY --from=builder /build/migrations /app/migrations

ENV APP_INSTANCE_NAME=auren \
    APP_HOSTNAME=auren \
    APP_PORT=8080 \
    ANNOUNCER_PORT=7777 \
    APP_ASSETS_DIR=/static \
    AUREN_BIND_ADDR=0.0.0.0:8080 \
    AUREN_DATABASE_URL=sqlite:///data/auren.db?mode=rwc \
    AUREN_FRONTEND_DIR=/static \
    AUREN_BUILTIN_SKILLS_DIR=/app/skills \
    AUREN_SKILLS_DIR=/data/skills \
    AUREN_WORKSPACE_DIR=/workspace \
    AI_WORKSPACE_CONTAINER_PATH=/workspace

RUN mkdir -p /data/skills /workspace

EXPOSE 8080

# Health is served by the app itself; the announcer only supervises it.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8080/health/ready >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/app/app-announcer", "/app/auren-server"]
