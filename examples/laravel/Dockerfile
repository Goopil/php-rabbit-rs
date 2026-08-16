# Multi-stage Dockerfile for a Laravel application using Rabbit RS.
#
# This Dockerfile installs the native extension via PIE and the Laravel bridge
# via Composer. No dedicated Rabbit RS image is needed.
#
# Usage:
#   docker build -t my-app .
#   docker run my-app php artisan queue:work --connection=rabbit-rs

# ─── Stage 1: Build the application ──────────────────────────────────────────

FROM php:8.4-cli AS builder

# Install system dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    libzip-dev \
    unzip \
    git \
    && rm -rf /var/lib/apt/lists/*

# Install Composer
COPY --from=composer:latest /usr/bin/composer /usr/bin/composer

# Install PIE (PHP Installer for Extensions)
RUN curl -L https://github.com/php/pie/releases/latest/download/pie.phar -o /usr/local/bin/pie \
    && chmod +x /usr/local/bin/pie

# Install the Rabbit RS native extension
RUN pie install goopil/rabbit-rs-native

# Verify the extension is loaded
RUN php --ri rabbit_rs

# Copy application source
WORKDIR /app
COPY composer.json composer.lock ./
RUN composer install --no-dev --optimize-autoloader --no-scripts

COPY . .
RUN composer dump-autoload --optimize

# ─── Stage 2: Production image ──────────────────────────────────────────────

FROM php:8.4-cli AS production

# Install runtime system dependencies (minimal)
RUN apt-get update && apt-get install -y --no-install-recommends \
    libzip4 \
    && rm -rf /var/lib/apt/lists/*

# Copy the installed extension from the builder stage
# PIE installs the .so and enables it in the PHP config
COPY --from=builder /usr/local/lib/php/extensions/ /usr/local/lib/php/extensions/
COPY --from=builder /usr/local/etc/php/conf.d/ /usr/local/etc/php/conf.d/

# Copy the application
WORKDIR /app
COPY --from=builder /app /app

# Verify the extension is loaded in the production image
RUN php -m | grep rabbit_rs

# Default command — override with docker run
CMD ["php", "artisan", "rabbit-rs:work", "--workers=4"]
