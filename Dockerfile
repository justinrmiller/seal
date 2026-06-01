# Multi-stage Rust build for DigitalOcean App Platform.
# The release binary is self-contained — templates/, static/, and config.yaml
# are bundled in via include_dir!/include_str! at build time.

FROM rust:1-bookworm AS builder
WORKDIR /build

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      protobuf-compiler \
      cmake \
      pkg-config \
      libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY static ./static
COPY config.yaml ./config.yaml

RUN cargo build --release --bin seal-server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /app/data

WORKDIR /app
COPY --from=builder /build/target/release/seal-server /usr/local/bin/seal-server

ENV APP_HOST=0.0.0.0
ENV APP_PORT=8080
ENV DATABASE_PATH=/app/data/chat.lance
EXPOSE 8080

CMD ["seal-server"]
