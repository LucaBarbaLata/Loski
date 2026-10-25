# syntax=docker/dockerfile:1.7

FROM rust:1.99.1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/loski /usr/local/bin/loski

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data loski \
    && mkdir -p /data && chown loski /data
COPY --from=build /usr/local/bin/loski /usr/local/bin/loski
USER loski
ENV DATA_DIR=/data
VOLUME /data
# 5588: login page and your Spotify app's callback. 8898: librespot's login callback.
EXPOSE 5588 8898
CMD ["loski"]
