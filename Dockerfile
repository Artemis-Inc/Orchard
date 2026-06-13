# Build the `orch` CLI and ship it in a tiny runtime image.
#
#   docker build -t orchard .
#   docker run --rm orchard --version
#
# The release workflow publishes this image to ghcr.io/artemis-inc/orchard.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p orch-cli

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/orch /usr/local/bin/orch
# Run agents from a mounted /work directory by default.
WORKDIR /work
ENTRYPOINT ["orch"]
CMD ["--help"]
