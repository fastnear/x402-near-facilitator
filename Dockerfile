# syntax=docker/dockerfile:1.7

FROM rust:1.97-bookworm@sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa AS builder
WORKDIR /src

COPY .cargo .cargo
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY migrations migrations

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo install cargo-auditable --version 0.7.5 --locked && \
    cargo auditable build --release --locked \
      --package x402-near-facilitator --bins && \
    mkdir -p /out && \
    install -m 0755 target/release/x402-near-facilitator /out/x402-near-facilitator && \
    install -m 0755 target/release/x402-near-admin /out/x402-near-admin

FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS runtime

ARG VERSION=0.0.0
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="FastNEAR x402 facilitator for NEAR" \
      org.opencontainers.image.description="Rust x402 v2 exact Circle USDC facilitator for NEAR" \
      org.opencontainers.image.source="https://github.com/fastnear/x402-near-facilitator" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}" \
      org.opencontainers.image.licenses="Apache-2.0"

RUN addgroup --system facilitator && \
    adduser --system --ingroup facilitator --no-create-home facilitator

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /out/x402-near-facilitator /usr/local/bin/
COPY --from=builder /out/x402-near-admin /usr/local/bin/
COPY migrations /usr/share/x402-near-facilitator/migrations
COPY LICENSE NOTICE /usr/share/doc/x402-near-facilitator/

USER facilitator:facilitator
WORKDIR /var/empty
ENTRYPOINT ["/usr/local/bin/x402-near-facilitator"]
CMD ["--config", "/etc/x402-near-facilitator/config.json"]
