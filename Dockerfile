# syntax=docker/dockerfile:1

# renovate: datasource=crate depName=cargo-chef
ARG CARGO_CHEF_VERSION=0.1.78

# The Rust version is the one `mise.toml` pins: bump both together.
FROM rust:1.100.0-trixie@sha256:e84068000361bf3d4a5ba2b7647f62893528b1eb9f847d7a23b493f5ce8ea254 AS chef
ARG CARGO_CHEF_VERSION
WORKDIR /usr/local/src/oas2mcp
# `musl-gcc` compiles the C in aws-lc-sys, ring and mimalloc for the musl
# target. Each architecture builds natively, so `uname -m` names the target.
# hadolint ignore=DL3008
RUN apt-get update \
 && apt-get install --yes --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add "$(uname -m)-unknown-linux-musl" \
 && cargo install cargo-chef --locked --version "${CARGO_CHEF_VERSION}"

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /usr/local/src/oas2mcp/recipe.json recipe.json
RUN cargo chef cook --release --target "$(uname -m)-unknown-linux-musl" \
      --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --target "$(uname -m)-unknown-linux-musl" \
 && cp "target/$(uname -m)-unknown-linux-musl/release/oas2mcp" /usr/local/bin/oas2mcp

# The binary is static (musl), so the runtime needs no libc: only the TLS roots
# and the nonroot user (65532:65532) the `static` image bakes in.
FROM gcr.io/distroless/static-debian13:nonroot@sha256:8ce6dd87e9823c5b48dcfb2c8604714ccb351ce59607327bcc4957448da279b5 AS runtime
COPY --from=builder /usr/local/bin/oas2mcp /usr/local/bin/oas2mcp

USER 65532:65532
# The transport is a subcommand (`http`, `sse`), `stdio` when none is given;
# a remote one listens on every interface so a published port reaches it.
ENV BIND_ADDR=0.0.0.0:8000
EXPOSE 8000
HEALTHCHECK CMD ["/usr/local/bin/oas2mcp", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/oas2mcp"]
