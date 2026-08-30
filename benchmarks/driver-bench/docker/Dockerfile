# Driver-bench image for the ext-amqp driver (iamfarhad/laravel-rabbitmq).
#
# ext-amqp also exists locally (Homebrew php@8.4 ships 2.2.0, and 2.2.0
# builds against 8.5 via phpize) — see the README cross-check. This image
# remains the canonical reproducible environment for the rabbitmq-ext
# connection. The vendor/ tree is NOT baked into the image: the driver-bench
# directory is bind-mounted at runtime and composer install runs against the
# mount, so the same lock file is shared between the local runs and the
# container.
#
# The container runs as a non-root user (uid/gid 1000). Docker Desktop
# handles bind-mount permissions transparently; on Linux hosts, either match
# the ownership of the mounted directory (chown -R 1000:1000) or override
# with --user "$(id -u):$(id -g)".
#
# Build:
#   docker build -t rabbit-rs-driver-bench ./docker
#
# Run (Linux, or Docker Desktop with host networking enabled):
#   docker run --rm -it --network host -v "$PWD":/app -w /app \
#     rabbit-rs-driver-bench php bin/bench.php --connection=rabbitmq-ext \
#     --mode=dispatch --count=1000
#
# The broker lab must be reachable at 127.0.0.1:5672 (see lab/compose.yaml).

FROM php:8.4-cli

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git \
        librabbitmq-dev \
        unzip \
    && groupadd --gid 1000 app \
    && useradd --uid 1000 --gid app --create-home --shell /bin/bash app \
    && pecl install amqp-2.2.0 \
    && docker-php-ext-enable amqp \
    && rm -rf /var/lib/apt/lists/*

COPY --from=composer:2 /usr/bin/composer /usr/bin/composer

USER app

WORKDIR /app
