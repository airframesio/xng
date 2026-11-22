FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libclang-dev protobuf-compiler cmake \
    libsoapysdr-dev libopus-dev \
    && rm -rf /var/lib/apt/lists/*

# libopus-dev: the opus feature links Debian's libopus instead of building
# the bundled copy, whose NEON code gcc rejects on armv7 ("inlining failed
# ... target specific option mismatch").
WORKDIR /usr/src/app
COPY . .
# Local speech-to-text (sherpa-onnx) so `--transcribe` works out of the box.
# sherpa-onnx ships no 32-bit ARM library, so the armv7 image goes without it
# (transcripts there need --whisper-url).
ARG TARGETARCH
RUN if [ "$TARGETARCH" = "arm" ]; then FEATURES=""; else FEATURES="--features sherpa"; fi \
    && cargo build --release --bin xng $FEATURES

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsoapysdr0.8 soapysdr-module-all libopus0 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/xng /usr/local/bin/xng

# Downloaded speech-to-text models live here; the volume keeps them across
# container restarts (or mount your own: -v xng-models:/var/cache/xng).
ENV XDG_CACHE_HOME=/var/cache
VOLUME /var/cache/xng

ENTRYPOINT ["xng"]
CMD ["--version"]
