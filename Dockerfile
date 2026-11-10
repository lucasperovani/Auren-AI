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
# Build with Cargo's output kept OUT of the image layers
#
# The `target/` directory of a release build is several GB. With the `vfs`
# storage driver every layer is a *full copy* of the rootfs, so a multi-GB
# target directory left in a layer gets copied once per following instruction
# and exhausts the runner disk ("no space left on device").
#
# Mounting `target/` as a cache keeps it on a host directory instead, so layers
# only ever contain the final binary. The cache is keyed by mount path and
# survives between builds, which also preserves the compiled dependency graph —
# that is what the old stub-source trick was for, done properly.
#
# The binary is copied to `/out` in the same RUN because a cache mount is not
# part of the resulting image: it cannot be COPYed from afterwards.
# ---------------------------------------------------------------------------
COPY Cargo.toml Cargo.lock ./
# Sources must be present before the build, and migrations are embedded at
# compile time by sqlx::migrate!, so they are copied here too.
COPY crates ./crates
COPY migrations ./migrations

# LTO + opt-level=z + panic=abort + symbol stripping come from the release
# profile in Cargo.toml, so the output is already the smallest codegen.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target ${TARGET} -p auren-server \
 && mkdir -p /out \
 && cp "target/${TARGET}/release/auren-server" /out/auren-server

# ---------------------------------------------------------------------------
# Compression stage: UPX on a static musl binary typically halves it.
# Kept in its own stage so the UPX toolchain never reaches the runtime image.
#
# Pinned to arm64: UPX is an executable that rewrites the binary, so it must
# run on the architecture of the binary it compresses.
# ---------------------------------------------------------------------------
FROM --platform=linux/arm64 alpine:3.21 AS compress
RUN apk add --no-cache upx
COPY --from=builder /out/auren-server /auren-server
# --best is slow (seconds) but yields the smallest output; lzma is tighter than
# the default. `--force` is required for static binaries.
RUN upx --best --lzma --force /auren-server

# ---------------------------------------------------------------------------
# The announcer image, referenced as a stage so `COPY --from=announcer` can pull
# files out of it in the runtime stage.
# ---------------------------------------------------------------------------
FROM --platform=linux/arm64 registry.gitlab.com/lucas.perovani/app-announcer:latest AS announcer

# ---------------------------------------------------------------------------
# Runtime
#
# The published app-announcer image is built `FROM scratch`: it holds only
# `/app/app-announcer`, `/app/assets` and the CA bundle — there is no shell, no
# `/bin`, and therefore no `RUN` can execute inside it. Any `RUN` against that
# base fails with:
#
#     fork/exec /bin/sh: no such file or directory
#
# Its binary is statically linked, so it runs on any Linux rootfs. This stage
# therefore uses Alpine as the base and copies the announcer *out* of the
# original image. That keeps the announcer (and its mDNS supervision) while
# restoring a shell, `apk`, and busybox `wget` for the health check.
#
# Alpine is also what makes Chromium possible at all: it can only be installed
# by a package manager.
# ---------------------------------------------------------------------------
FROM --platform=linux/arm64 alpine:3.21 AS runtime
ARG WITH_BROWSER=false

# ca-certificates is always installed: the announcer points SSL_CERT_FILE at the
# system bundle, and outbound HTTPS from the server needs it.
#
# Chromium is ~180 MB, more than everything else combined, so it is opt-in.
# When absent the server detects that at startup and *hides* the tools that need
# it: the agent is never offered `web_navigate`, so a user cannot hit a silent
# failure. Alpine installs `/usr/bin/chromium-browser`.
RUN apk add --no-cache ca-certificates \
 && if [ "$WITH_BROWSER" = "true" ]; then \
        apk add --no-cache chromium; \
    else \
        echo "Chromium skipped (WITH_BROWSER=false); web_navigate will not be offered"; \
    fi

# The announcer binary and the assets it serves (`/icon`, `app_info.json`).
COPY --from=announcer /app/app-announcer /app/app-announcer
COPY --from=announcer /app/assets /app/assets

COPY --from=compress /auren-server /app/auren-server
# The SPA is served from disk, so the assets ship inside the image.
COPY frontend /static
# Built-in skill packages are read at runtime when a user installs one.
COPY skills /app/skills
# Migrations are NOT copied: `sqlx::migrate!` embeds them in the binary at
# compile time, so shipping the directory again would only duplicate data.

ENV APP_INSTANCE_NAME=auren \
    APP_HOSTNAME=auren \
    APP_PORT=8080 \
    ANNOUNCER_PORT=7777 \
    APP_ASSETS_DIR=/app/assets \
    AUREN_BIND_ADDR=0.0.0.0:8080 \
    AUREN_DATABASE_URL=sqlite:///data/auren.db?mode=rwc \
    AUREN_FRONTEND_DIR=/static \
    AUREN_BUILTIN_SKILLS_DIR=/app/skills \
    AUREN_SKILLS_DIR=/data/skills \
    AUREN_WORKSPACE_DIR=/workspace \
    AI_WORKSPACE_CONTAINER_PATH=/workspace

# These two were inherited from the original base image's ENV and are NOT
# defaults in the announcer binary. Switching the base to Alpine silently loses
# them, which would break mDNS discovery and certificate verification, so they
# are restated explicitly.
#
#   APP_MDNS_SERVICE_TYPE  the mDNS service type the announcer registers
#   SSL_CERT_FILE          bundle used for outbound TLS by the Rustls stack
ENV APP_MDNS_SERVICE_TYPE=_app._tcp.local. \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

# NOTE: APP_ASSETS_DIR points at the *announcer's* assets, not at /static.
# The announcer serves `/icon` and `/app_info` from there, so pointing it at the
# SPA directory would only ever return its 1x1 placeholder. The server has its
# own variable (AUREN_FRONTEND_DIR) for the SPA.
RUN mkdir -p /data/skills /workspace

EXPOSE 8080

# Health is served by the app itself; the announcer only supervises it.
# busybox `wget` comes with Alpine, so this works — it did not exist on the
# scratch base, where the container could never report healthy.
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8080/health/ready >/dev/null 2>&1 || exit 1

ENTRYPOINT ["/app/app-announcer", "/app/auren-server"]
