FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libclang-dev protobuf-compiler \
    libsoapysdr-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/app
COPY . .
RUN cargo build --release --bin xng

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsoapysdr0.8 soapysdr-module-all ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/xng /usr/local/bin/xng

ENTRYPOINT ["xng"]
CMD ["--version"]
