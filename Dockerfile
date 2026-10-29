# syntax=docker/dockerfile:1.7

FROM rust:1.99.1-bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
# Piper speaks Loski's replies to voice commands (talking back, beta). It's
# checked against the release it comes from before it goes in the image.
RUN mkdir -p /opt/piper \
    && case "$(uname -m)" in \
        x86_64) arch=x86_64 sum=a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992 ;; \
        aarch64) arch=aarch64 sum=fea0fd2d87c54dbc7078d0f878289f404bd4d6eea6e7444a77835d1537ab88eb ;; \
        *) exit 0 ;; \
    esac \
    && curl -fsSL -o /tmp/piper.tar.gz \
        "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_${arch}.tar.gz" \
    && echo "$sum  /tmp/piper.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/piper.tar.gz -C /opt \
    && rm /tmp/piper.tar.gz
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
COPY --from=build /opt/piper /opt/piper
# Loski starts as root only to hand /data to its own user (LOSKI_UID, 10001
# by default), then runs as that user; see src/user.rs.
ENV DATA_DIR=/data HOME=/data
VOLUME /data
# 5588: login page and your Spotify app's callback. 8898: librespot's login callback.
EXPOSE 5588 8898
CMD ["loski"]
