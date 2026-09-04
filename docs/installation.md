# Installation

This guide covers installing the Rabbit RS native extension and the Laravel bridge.

## Prerequisites

- PHP 8.4 or 8.5 (**NTS only** — ZTS is not supported in V1, see [Thread safety](#thread-safety))
- Linux x86_64 or ARM64 (glibc or musl)
- RabbitMQ 4.3.x (reachable from your PHP process)
- [PIE](https://github.com/php/pie) 1.5+ for extension installation
- [Composer](https://getcomposer.org) for the Laravel bridge

> **macOS and Windows** are not supported as production platforms in V1. You can compile and test locally on macOS for development purposes, but pre-compiled binaries target Linux only.

### Thread safety

V1 ships **NTS binaries only**. PIE will not match a ZTS PHP installation (`composer.json` declares `"support-zts": false`). This is deliberate: the extension keeps a process-global runtime and connection registry, and TSRM per-thread isolation is not implemented in V1, so ZTS binaries would share that registry across PHP threads without synchronization. ZTS support is planned for V2 with per-thread isolation, a blocking ZTS CI job, and real concurrency tests — see [Distribution](distribution.md#thread-safety-nts-only-in-v1).

## Step 1 — Install the native extension

```bash
pie install goopil/rabbit-rs-native
```

PIE selects the correct pre-compiled binary for your environment:

- PHP version (8.4 or 8.5)
- Architecture (x86_64 or arm64)
- libc (glibc or musl)
- Thread safety (NTS only in V1)

It copies the shared object (`rabbit_rs.so`) to your PHP extension directory and enables it in the active PHP configuration.

### Verify installation

```bash
php --ri rabbit_rs
```

Expected output:

```
rabbit_rs

Rabbit RS - High-performance RabbitMQ transport for PHP and Laravel, powered by Rust
Version => 0.0.9
...
```

### Dockerfile usage

Use PIE in a multi-stage Dockerfile. No dedicated Rabbit RS image is needed:

```dockerfile
FROM php:8.4-cli AS base

# Install PIE
RUN curl -L https://github.com/php/pie/releases/latest/download/pie.phar -o /usr/local/bin/pie \
    && chmod +x /usr/local/bin/pie

# Install the extension
RUN pie install goopil/rabbit-rs-native

# Verify
RUN php --ri rabbit_rs

# Install Composer and the Laravel bridge
COPY --from=composer:latest /usr/bin/composer /usr/bin/composer
RUN composer require goopil/rabbit-rs-laravel

# ... your application
```

For a complete Dockerfile example, see [examples/laravel/Dockerfile](../examples/laravel/Dockerfile).

## Step 2 — Install the Laravel bridge

```bash
composer require goopil/rabbit-rs-laravel
```

Composer installs the PHP package and verifies that `ext-rabbit_rs` is loaded. It does **not** install or modify system PHP binaries — that is PIE's job.

The package auto-discovers the service provider in Laravel 12 and 13. If you disabled auto-discovery, register it manually:

```php
// config/app.php
'providers' => [
    // ...
    Goopil\RabbitRs\Laravel\RabbitMqServiceProvider::class,
],
```

## Step 3 — Publish the configuration

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

This creates `config/rabbit-rs.php` with sensible defaults. See [Configuration](configuration.md) for the full reference.

## Step 4 — Verify the installation

```bash
php artisan rabbit-rs:status
```

This displays connection state, pool metrics, and consumer stats. For machine-readable output:

```bash
php artisan rabbit-rs:status --format=json
```

## Local compilation with Cargo

For contributors or environments without PIE:

```bash
# Clone the repository
git clone https://github.com/Goopil/rabbit-rs.git
cd rabbit-rs

# Build the extension in release mode
cargo build --release -p rabbit-rs-php

# Install into the current PHP
./scripts/install.sh --release
```

The `install.sh` script wraps `cargo php install` with the correct manifest path (the workspace root `Cargo.toml` is workspace-only, so `cargo-php` needs the package manifest at `crates/rabbit-rs-php/Cargo.toml`).

### Requirements for local compilation

- Rust 1.96.0 (pinned in `rust-toolchain.toml`)
- `cargo-php` (install with `cargo install cargo-php`)
- PHP 8.4 or 8.5 with development headers
- `libssl-dev` (or `openssl-devel` / `openssl-dev` depending on your distro)

## Why Composer doesn't modify system PHP

The native extension is a binary shared object (`rabbit_rs.so`) that must be compiled for your specific PHP version, architecture, libc, and thread-safety mode. Composer is a PHP dependency manager — it handles PHP source packages, not system binaries.

The separation is:

| Tool | Responsibility |
|------|---------------|
| PIE | Downloads and installs the correct pre-compiled `.so` binary |
| Composer | Installs the Laravel bridge (PHP source) and verifies `ext-rabbit_rs` is loaded |

The Laravel bridge's `composer.json` declares `"ext-rabbit_rs": "^0.0"`, which causes Composer to check that the extension is loaded at install time. If the extension is missing, Composer reports the error. But Composer never installs the binary — that is PIE's role.

## Multiple PHP versions

If you have multiple PHP installations, PIE and `cargo-php` target the PHP found in your `PATH`. To target a specific PHP:

```bash
# With PIE (uses the php-config/phpize in PATH)
/path/to/php/bin/php /usr/local/bin/pie install goopil/rabbit-rs-native

# With cargo-php
PHP_CONFIG=/path/to/php-config ./scripts/install.sh --release
```

## Upgrading and rollback

PIE installs are versioned replacements: installing a different version swaps the `rabbit_rs.so` binary and updates the active PHP configuration in place. Upgrades and rollbacks use the same `pie install` command with an explicit release tag.

Upgrade (or reinstall) an exact version:

```bash
pie install goopil/rabbit-rs-native:v0.1.1
```

Rollback = install the previous tag:

```bash
pie install goopil/rabbit-rs-native:v0.1.0
```

Check which version is active before and after:

```bash
php --ri rabbit_rs
```

Keep the Laravel bridge in sync: `goopil/rabbit-rs-laravel` requires a specific `ext-rabbit_rs` major version. When moving across a major boundary — in either direction — upgrade or roll back the extension and the bridge together. Composer fails loudly at `composer update` if the loaded extension does not satisfy the bridge's constraint, so a half-upgraded system (new bridge with old extension, or the reverse) cannot go unnoticed.

Every release exercises these paths in CI before it is finalized: the release pipeline installs the previous published release, upgrades it to the new release, and rolls back again (see [Distribution](distribution.md#end-to-end-pie-validation)).

## Next steps

- [Configuration reference](configuration.md)
- [Laravel usage](laravel.md)
- [Topology management](topology.md)
- [Reliability](reliability.md)
