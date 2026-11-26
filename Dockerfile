# syntax=docker/dockerfile:1

# renovate: datasource=crate depName=cargo-chef
ARG CARGO_CHEF_VERSION=0.1.78

# --- chef: cargo-chef base ---------------------------------------------------
# The Rust version is the one `mise.toml` pins: bump both together.
FROM rust:1.100.0-trixie AS chef
ARG CARGO_CHEF_VERSION
WORKDIR /usr/local/src/oas2mcp
RUN cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"

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
# `cc` (not `static`) because the binary links glibc (aws-lc-sys). Same Debian
# release as the builder, so the binary never needs a newer glibc than the
# runtime has. TLS roots and a nonroot user (65532:65532) are baked in.
FROM gcr.io/distroless/cc-debian13:nonroot AS runtime
COPY --from=builder \
     /usr/local/src/oas2mcp/target/release/oas2mcp /usr/local/bin/oas2mcp

USER 65532:65532
# The transport is a subcommand (`http`, `sse`), `stdio` when none is given;
# a remote one listens on every interface so a published port reaches it.
ENV BIND_ADDR=0.0.0.0:8000
EXPOSE 8000
HEALTHCHECK CMD ["/usr/local/bin/oas2mcp", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/oas2mcp"]
