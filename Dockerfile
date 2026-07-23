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

# --- runtime: distroless -----------------------------------------------------
# `cc` (not `static`) because the binary links glibc + libssl-style C deps
# (aws-lc-sys). Distroless ships no shell, no package manager and no OS package
# layer, so an image scanner finds essentially nothing to flag — unlike
# debian:bookworm-slim, whose ~20 unfixed HIGH/CRITICAL advisories we used to
# carry. TLS roots (ca-certificates) and a nonroot user (uid 65532) are baked
# into the image.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder \
     /usr/local/src/oas2mcp/target/release/oas2mcp /usr/local/bin/oas2mcp

USER nonroot
# Default to the remote transport; override TRANSPORT/BIND_ADDR as needed.
ENV TRANSPORT=streamable-http \
    BIND_ADDR=0.0.0.0:8000
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/oas2mcp"]
