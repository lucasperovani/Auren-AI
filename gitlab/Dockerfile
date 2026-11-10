# Build do cliente (wasm) e do servidor (musl estático, aarch64)
# Rust 1.97: mesma toolchain usada para validar o build localmente; o
# Cargo.lock do cargo-web vendorado foi gerado com cargo 1.97 e contém
# versões (encoding_rs/icu_*) que exigem rustc >= 1.88.
FROM docker.io/library/rust:1.97-alpine AS builder
ARG TARGET=aarch64-unknown-linux-musl

# Dependências para compilar openssl vendored e dependências git
# cargo-web é compilado a partir do vendor/ (a versão do crates.io está
# abandonada e quebra com cargo/rustc modernos; a vendorada tem fixes para
# package IDs modernos, parity-wasm com trunc_sat/bulk-memory final-spec/
# call_indirect varuint32 e name sections novas).
RUN apk add --no-cache musl-dev perl make git ca-certificates \
 && rustup target add ${TARGET} wasm32-unknown-unknown

WORKDIR /build
COPY . .

# Instala o cargo-web vendorado (precisa do código-fonte copiado primeiro;
# vai para $CARGO_HOME/bin, que já está no PATH da imagem oficial)
RUN cargo install --path vendor/cargo-web --locked

# Cliente wasm (cargo-web) + servidor estático musl
RUN cargo web deploy --package tetris_client --release \
 && RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target ${TARGET} --package tetris_server

# Runtime: imagem base do app-announcer (arm64)
FROM registry.gitlab.com/lucas.perovani/app-announcer:latest
ARG TARGET=aarch64-unknown-linux-musl

COPY --from=builder /build/target/${TARGET}/release/tetris_server /tetris_server

# Place public assets under /static (served by tetris_server)
COPY ./static /static

# Web artifacts (wasm/js) belong in /static
COPY --from=builder /build/target/deploy/tetris_client.* /static/

ENV APP_INSTANCE_NAME=tetris \
    APP_HOSTNAME=tetris \
    APP_PORT=8089 \
    APP_ASSETS_DIR=/static

EXPOSE 8089
ENTRYPOINT ["/app/app-announcer", "/tetris_server"]
