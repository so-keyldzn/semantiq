# Semantiq HTTP API Docker image: the server indexes a copy of this
# repository (the sources of the build itself, not a clone of `main`) and
# serves the REST API + MCP Streamable HTTP on $HTTP_PORT.
#
#   docker build -t semantiq .
#   docker run --rm -p 8080:8080 semantiq
#   curl http://localhost:8080/health
#
# The build context is trimmed by .dockerignore (no target/, .git, index DBs).

# ==========================================
# Stage 1: dependency recipe (cargo-chef)
# ==========================================
# `cargo chef cook` compiles the dependency graph from the manifests alone, so
# that layer survives source edits: a change under crates/ only rebuilds the
# workspace crates, not the ~360 dependencies (ort downloads the ONNX Runtime
# library in its build script, which is part of that cached layer).
FROM rust:1-slim-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ==========================================
# Stage 2: build the binary
# ==========================================
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --bin semantiq --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# Embedded by `semantiq init` (include_str!).
COPY skills ./skills
# --locked: a Cargo.lock out of step with Cargo.toml fails the build instead
# of being resolved silently to other versions than the ones CI tested.
RUN cargo build --release --locked --bin semantiq

# ==========================================
# Stage 3: runtime image
# ==========================================
FROM debian:bookworm-slim

# ca-certificates: the embedding model download; curl: the health check.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Non-root user for the server.
RUN groupadd --gid 1000 semantiq && \
    useradd --uid 1000 --gid semantiq --shell /bin/sh --create-home semantiq

WORKDIR /app

COPY --from=builder /build/target/release/semantiq /usr/local/bin/semantiq

# The project served by the demo: the sources this image was built from.
COPY --chown=semantiq:semantiq Cargo.toml Cargo.lock README.md CLAUDE.md CHANGELOG.md /app/semantiq-repo/
COPY --chown=semantiq:semantiq crates /app/semantiq-repo/crates
COPY --chown=semantiq:semantiq skills /app/semantiq-repo/skills

# The index (.semantiq.db in the project) and the model cache live under /app.
RUN mkdir -p /app/data && chown -R semantiq:semantiq /app

EXPOSE 8080

# HTTP_HOST must be a non-loopback address: inside a container, 127.0.0.1
# (the server's default) is unreachable from the published port while the
# health check, run inside the container, stays green.
ENV RUST_LOG=info
ENV HTTP_PORT=8080
ENV HTTP_HOST=0.0.0.0
# Embedding model cache (~140 MB, downloaded on the first start): mount a
# volume here to keep it across containers.
ENV XDG_DATA_HOME=/app/data

# Shell form so the port follows HTTP_PORT.
HEALTHCHECK --interval=30s --timeout=3s --start-period=60s --retries=3 \
    CMD curl -f "http://127.0.0.1:${HTTP_PORT}/health" || exit 1

USER semantiq

# Index the structure first, then serve (embeddings continue in the background).
CMD ["sh", "-c", "semantiq index /app/semantiq-repo --no-embeddings && exec semantiq serve --project /app/semantiq-repo --http-host ${HTTP_HOST} --http-port ${HTTP_PORT}"]
