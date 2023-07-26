FROM rust:1.70-bookworm

RUN apt-get update
RUN apt-get install pkg-config libssl-dev -y
RUN apt-get install libsoapysdr-dev python3-soapysdr soapysdr-module-all soapysdr-tools  -y

WORKDIR /usr/src/app
COPY . .
RUN cargo install --path .

CMD ["xng"]
