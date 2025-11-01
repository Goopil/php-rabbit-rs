# syntax=docker/dockerfile:1.5
##
# Build RabbitRs PHP extension against Alpine-based PHP images.
# Usage (example):
#   docker build -f docker/linux-alpine/Dockerfile \
#     --build-arg PHP_VERSION=8.2 \
#     --build-arg RUST_VERSION=1.89.0 \
#     -t rabbit-rs-linux-alpine:8.2 .
#
# The resulting image exposes the compiled module at /artifacts/rabbit_rs.so.
##

ARG PHP_VERSION=8.2
ARG PHP_VARIANT=cli-alpine3.22
ARG RUST_VERSION=1.89.0

FROM php:${PHP_VERSION}-${PHP_VARIANT} AS build

ARG RUST_VERSION
ENV PATH="/root/.cargo/bin:${PATH}" \
    EXT_PHP_RS_PHP_CONFIG=/usr/local/bin/php-config

RUN apk add --no-cache \
      build-base \
      pkgconfig \
      autoconf \
      automake \
      libtool \
      curl \
      git

RUN phpize --version && php-config --version

RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain "${RUST_VERSION}"

WORKDIR /src
COPY . .

RUN cargo build --release --locked

FROM alpine:3.19 AS artifact

RUN mkdir -p /artifacts

COPY --from=build /src/target/release/librabbit_rs.so /artifacts/rabbit_rs.so
