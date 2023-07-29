FROM rust:bookworm AS builder

RUN apt-get update
RUN apt-get install -y pkg-config libssl-dev libclang-dev
RUN apt-get install -y libsoapysdr-dev python3-soapysdr soapysdr-module-all soapysdr-tools

WORKDIR /usr/src/app
COPY . .
RUN cargo install --path .

CMD ["xng", "--version"]
