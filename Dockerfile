# Semantiq HTTP API Docker image: the server serves the REST API + MCP
# Streamable HTTP on $HTTP_PORT over a copy of this repository (the sources
# of the build itself, not a clone of `main`), indexed at build time with the
# embedding model, so a container answers as soon as it starts.
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
# Embedding model cache (dirs::data_dir()/semantiq/models).
ENV XDG_DATA_HOME=/app/data

USER semantiq

# Index the demo project here, embeddings included. `semantiq serve` loads
# the embedding model before it binds the HTTP listener (RetrievalEngine::new
# downloads it, ~140 MB, when the cache is empty): without this step, the
# first start of a container would spend that download, possibly longer than
# the health check's start period, before /health answers. The image ships the
# model and the index instead; the server only refreshes them and needs no
# network. The `test` fails the build when the model did not download (the
# index command only warns and leaves the chunks unembedded).
RUN semantiq index /app/semantiq-repo \
    && test -s /app/data/semantiq/models/coderankembed-int8.onnx

# Shell form so the port follows HTTP_PORT.
HEALTHCHECK --interval=30s --timeout=3s --start-period=60s --retries=3 \
    CMD curl -f "http://127.0.0.1:${HTTP_PORT}/health" || exit 1

# Shell form so the address and port follow HTTP_HOST / HTTP_PORT.
CMD ["sh", "-c", "exec semantiq serve --project /app/semantiq-repo --http-host ${HTTP_HOST} --http-port ${HTTP_PORT}"]
