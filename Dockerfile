# Semantiq HTTP API Docker Image
# Multi-stage build for minimal image size

# ==========================================
# Stage 1: Build the Rust binary
# ==========================================
FROM rust:1.87-slim-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build in release mode
RUN cargo build --release --bin semantiq

# ==========================================
# Stage 2: Runtime image
# ==========================================
FROM debian:bookworm-slim

# Install all runtime dependencies in a single layer
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for running the application
RUN groupadd --gid 1000 semantiq && \
    useradd --uid 1000 --gid semantiq --shell /bin/sh --create-home semantiq

WORKDIR /app

# Copy the binary
COPY --from=builder /build/target/release/semantiq /usr/local/bin/semantiq

# Clone the Semantiq repository for meta-demo (exploring its own code)
RUN git clone --depth 1 https://github.com/nicobarray/semantiq.git /app/semantiq-repo

# Create data directory for the index and set ownership
RUN mkdir -p /app/data && chown -R semantiq:semantiq /app

# Expose the HTTP port
EXPOSE 8080

# Environment variables (can be overridden)
ENV RUST_LOG=info
ENV HTTP_PORT=8080

# Health check (curl is installed above)
HEALTHCHECK --interval=30s --timeout=3s --start-period=60s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Switch to non-root user
USER semantiq

# Run the server
# First index, then serve
CMD ["sh", "-c", "semantiq index /app/semantiq-repo && semantiq serve --project /app/semantiq-repo --http-port ${HTTP_PORT}"]
