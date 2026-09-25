# CI base image for the extension build/test jobs (PHPT + FPM certification).
# Published to GHCR by .github/workflows/ci-base-image.yml, which triggers when
# this file changes (or on demand). Baking apt + rustup + composer here keeps
# those installs out of every CI run and out of GitHub API 504 blast radius.
#
# The toolchain pin must match .github/actions/rust-setup and the CI jobs
# (Rust 1.98.1): bump both together.
ARG PHP_VERSION=8.4
ARG PHP_FLAVOR=cli
FROM php:${PHP_VERSION}-${PHP_FLAVOR}

ARG RUST_VERSION=1.98.1

# Superset of the package lists the PHPT (cli) and FPM certification (fpm)
# jobs used to install on every run.
RUN apt-get update -qq \
    && apt-get install -y -qq libclang-dev curl gcc make unzip git jq \
    && rm -rf /var/lib/apt/lists/*

# Toolchain lives under /usr/local so the jobs' cache mounts
# (/usr/local/cargo/{registry,git}) never shadow the baked binaries.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:${PATH}

RUN curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "${RUST_VERSION}" --profile minimal \
    && chmod -R a+w "${RUSTUP_HOME}" "${CARGO_HOME}"

RUN curl -sS https://getcomposer.org/installer \
        | php -- --install-dir=/usr/local/bin --filename=composer
