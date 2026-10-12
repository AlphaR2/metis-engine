# syntax=docker/dockerfile:1
#
# metis-api (the aggregator). Build from the workspace root:
#     docker build -t metis-api .
#
# The whole workspace is copied because Cargo resolves every member's manifest, but only the
# aggregator and its dependencies are compiled.

FROM rust:1.98-bookworm AS build
WORKDIR /src
# Set FEATURES=devnet (a Railway service variable) to compile in the devnet program id and trust roots, so the
# Metis config address is derived with no further settings. Empty = the default build.
ARG FEATURES=""
COPY . .
# Cache mounts keep the registry and target dir between builds; the binary is copied out because
# the target dir is not part of the image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p metis-aggregator --bin metis-api ${FEATURES:+--features "$FEATURES"} \
    && mkdir -p /out && cp target/release/metis-api /out/metis-api

FROM debian:bookworm-slim AS runtime
# CA roots for the venues' and the RPC's TLS. Nothing else is needed at runtime.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home metis
COPY --from=build /out/metis-api /usr/local/bin/metis-api
USER metis
# Configuration comes from the environment (see aggregator/.env.example); no .env is baked in.
ENV PORT=8080 LOG_FORMAT=json RUST_LOG=info
EXPOSE 8080
# Liveness: GET /health. The image has no curl on purpose; probe it from the orchestrator.
# Exec form so SIGTERM reaches the process and it drains in-flight requests before exiting.
ENTRYPOINT ["metis-api"]
