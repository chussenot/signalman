# Two stages: compile on the pinned toolchain, run on a slim Debian with only
# CA certificates (reqwest uses rustls, so no OpenSSL) and curl for the
# HEALTHCHECK. The image serves by default; every other subcommand works as
# `docker run ... signalman <subcommand>`.
FROM rust:1.94.1-slim-bookworm AS build
# The image already carries this toolchain; stop rustup reading
# rust-toolchain.toml and fetching rustfmt and clippy for nothing.
ENV RUSTUP_TOOLCHAIN=1.94.1
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/signalman /signalman

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home signalman
COPY --from=build /signalman /usr/local/bin/signalman
USER 10001
# Listen on every interface; this is the env layer, above a config file's
# `server.addr` (decision 0006), so override it with the variable, not the file.
ENV SIGNALMAN_ADDR=0.0.0.0:8080
EXPOSE 8080
# Liveness only; readiness (upstream checks) is GET /readyz, for the orchestrator.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["signalman"]
CMD ["serve"]
