FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libclang-dev protobuf-compiler cmake \
    libsoapysdr-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app
COPY . .
# Airband transcription (sherpa-onnx) everywhere it has a prebuilt library;
# it has none for 32-bit ARM.
ARG TARGETARCH
RUN if [ "$TARGETARCH" = "arm" ]; then \
        cargo build --release --bin xng; \
    else \
        cargo build --release --bin xng --features sherpa; \
    fi

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsoapysdr0.8 soapysdr-module-all ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/xng /usr/local/bin/xng

ENTRYPOINT ["xng"]
CMD ["--version"]
