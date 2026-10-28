# syntax=docker/dockerfile:1.7

FROM rust:1.99.1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY src ./src
# The speech model for voice commands (whisper.cpp) is built for any x86
# processor with AVX2, or any 64-bit ARM, not just the machine building it.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    export GGML_NATIVE=OFF \
    && if [ "$(uname -m)" = x86_64 ]; then \
        export GGML_AVX=ON GGML_AVX2=ON GGML_FMA=ON GGML_F16C=ON; \
    fi \
    && cargo build --release --locked \
    && cp target/release/loski /usr/local/bin/loski

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data loski \
    && mkdir -p /data && chown loski /data
COPY --from=build /usr/local/bin/loski /usr/local/bin/loski
# Loski starts as root only to hand /data to its own user (LOSKI_UID, 10001
# by default), then runs as that user; see src/user.rs.
ENV DATA_DIR=/data HOME=/data
VOLUME /data
# 5588: login page and your Spotify app's callback. 8898: librespot's login callback.
EXPOSE 5588 8898
CMD ["loski"]
