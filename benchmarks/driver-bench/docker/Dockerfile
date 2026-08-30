# Driver-bench image for the ext-amqp driver (iamfarhad/laravel-rabbitmq).
#
# Local PHP builds (Homebrew) do not ship the amqp extension, so the
# rabbitmq-ext connection runs exclusively inside this container. The
# vendor/ tree is NOT baked into the image: the driver-bench directory is
# bind-mounted at runtime and composer install runs against the mount, so
# the same lock file is shared between the local runs and the container.
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
        librabbitmq-dev \
        git \
        unzip \
    && pecl install amqp \
    && docker-php-ext-enable amqp \
    && rm -rf /var/lib/apt/lists/*

COPY --from=composer:2 /usr/bin/composer /usr/bin/composer

WORKDIR /app
