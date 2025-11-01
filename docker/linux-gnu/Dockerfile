# syntax=docker/dockerfile:1.5
##
# Build RabbitRs PHP extension against glibc-based PHP images.
# Usage (example):
#   docker build -f docker/linux-gnu/Dockerfile \
#     --build-arg PHP_VERSION=8.2 \
#     --build-arg RUST_VERSION=1.89.0 \
#     -t rabbit-rs-linux-gnu:8.2 .
#
# The resulting image exposes the compiled module at /artifacts/rabbit_rs.so.
##

ARG PHP_VERSION=8.2
ARG PHP_VARIANT=cli
ARG RUST_VERSION=1.89.0

FROM php:${PHP_VERSION}-${PHP_VARIANT} AS build

ARG RUST_VERSION
ENV PATH="/root/.cargo/bin:${PATH}"

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential \
      pkg-config \
      git \
      curl \
      ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Ensure phpize / php-config exist (shivammathur/php ships with both)
RUN phpize --version && php-config --version

RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain "${RUST_VERSION}"

WORKDIR /src
COPY . .

ENV EXT_PHP_RS_PHP_CONFIG=/usr/local/bin/php-config

RUN cargo build --release --locked

FROM debian:bookworm-slim AS artifact

RUN mkdir -p /artifacts

COPY --from=build /src/target/release/librabbit_rs.so /artifacts/rabbit_rs.so
