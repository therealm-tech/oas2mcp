# syntax=docker/dockerfile:1

# --- chef: cargo-chef base ---------------------------------------------------
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /usr/local/src/oas2mcp

# --- planner: capture the dependency recipe ----------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- builder: cook dependencies, then build ----------------------------------
FROM chef AS builder
COPY --from=planner /usr/local/src/oas2mcp/recipe.json recipe.json
# Cached as long as dependencies do not change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked

# --- runtime -----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# TLS roots for fetching specs / proxying to HTTPS upstreams.
# hadolint ignore=DL3008
RUN apt-get update \
 && apt-get install --no-install-recommends -y ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN groupadd --system --gid 1000 oas2mcp \
 && useradd  --system --uid 1000 --gid oas2mcp \
      --home /etc/oas2mcp --shell /usr/sbin/nologin oas2mcp

COPY --from=builder --chown=oas2mcp:oas2mcp \
     /usr/local/src/oas2mcp/target/release/oas2mcp /usr/local/bin/oas2mcp

USER oas2mcp
# Default to the remote transport; override TRANSPORT/BIND_ADDR as needed.
ENV TRANSPORT=streamable-http \
    BIND_ADDR=0.0.0.0:8000
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/oas2mcp"]
