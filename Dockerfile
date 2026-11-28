# syntax=docker/dockerfile:1

# renovate: datasource=crate depName=cargo-chef
ARG CARGO_CHEF_VERSION=0.1.78

# The Rust version is the one `mise.toml` pins: bump both together.
FROM rust:1.100.0-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254 AS chef
ARG CARGO_CHEF_VERSION
WORKDIR /usr/local/src/oas2mcp
RUN cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /usr/local/src/oas2mcp/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked

# `cc` (not `static`) because the binary links glibc (aws-lc-sys). Same Debian
# release as the builder, so the binary never needs a newer glibc than the
# runtime has. TLS roots and a nonroot user (65532:65532) are baked in.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:29ee70762214fe8190cea04ef1c0a84a8365879e047ce8f911ae5a5ab2355fa2 AS runtime
COPY --from=builder \
     /usr/local/src/oas2mcp/target/release/oas2mcp /usr/local/bin/oas2mcp

USER 65532:65532
# The transport is a subcommand (`http`, `sse`), `stdio` when none is given;
# a remote one listens on every interface so a published port reaches it.
ENV BIND_ADDR=0.0.0.0:8000
EXPOSE 8000
HEALTHCHECK CMD ["/usr/local/bin/oas2mcp", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/oas2mcp"]
