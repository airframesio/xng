FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libclang-dev protobuf-compiler cmake \
    libsoapysdr-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app
COPY . .
# Local speech-to-text (sherpa-onnx) so `--transcribe` works out of the box.
# sherpa-onnx ships no 32-bit ARM library, so the armv7 image goes without it
# (transcripts there need --whisper-url).
#
# 32-bit ARM also cannot build the Opus source bundled with audiopus_sys: its
# CMake enables the NEON intrinsics files without -mfpu=neon, and GCC refuses
# them ("target specific option mismatch"). There xng links Debian's static
# libopus.a instead: LIBOPUS_LIB_DIR wins over building from source (it names
# a prefix, and audiopus_sys looks in its lib/), and OPUS_STATIC from
# .cargo/config.toml keeps it static, so the image still needs no libopus at
# run time.
ARG TARGETARCH
RUN set -e; \
    if [ "$TARGETARCH" = "arm" ]; then \
        apt-get update; \
        apt-get install -y --no-install-recommends libopus-dev; \
        rm -rf /var/lib/apt/lists/*; \
        mkdir -p /opt/libopus/lib; \
        ln -s "/usr/lib/$(gcc -dumpmachine)/libopus.a" /opt/libopus/lib/; \
        export LIBOPUS_LIB_DIR=/opt/libopus; \
        FEATURES=""; \
    else \
        FEATURES="--features sherpa"; \
    fi; \
    cargo build --release --bin xng $FEATURES

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsoapysdr0.8 soapysdr-module-all ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/xng /usr/local/bin/xng

# Downloaded speech-to-text models live here; the volume keeps them across
# container restarts (or mount your own: -v xng-models:/var/cache/xng).
ENV XDG_CACHE_HOME=/var/cache
# Station UUIDs (station-uuids.json) live on the volume, so they survive
# image updates.
ENV XNG_STATE_DIR=/var/cache/xng
VOLUME /var/cache/xng

ENTRYPOINT ["xng"]
CMD ["--version"]
