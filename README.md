# Rabbit RS

High-performance RabbitMQ transport for PHP and Laravel, powered by Rust.

[![CI](https://github.com/Goopil/rabbit-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/Goopil/rabbit-rs/actions/workflows/rust.yml)
[![PHP](https://github.com/Goopil/rabbit-rs/actions/workflows/php.yml/badge.svg)](https://github.com/Goopil/rabbit-rs/actions/workflows/php.yml)
[![Integration](https://github.com/Goopil/rabbit-rs/actions/workflows/integration.yml/badge.svg)](https://github.com/Goopil/rabbit-rs/actions/workflows/integration.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

## Quick start

**Step 1 — Install the native extension:**

```bash
pie install goopil/rabbit-rs-native
```

**Step 2 — Install the Laravel bridge:**

```bash
composer require goopil/rabbit-rs-laravel
```

**Step 3 — Publish the config:**

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

**Step 4 — Publish and consume a job:**

```php
// app/Jobs/ProcessOrder.php
class ProcessOrder implements ShouldQueue
{
    use Dispatchable, Queueable;

    public function __construct(public int $orderId) {}
}

// Dispatch
ProcessOrder::dispatch(42);
```

```bash
# Consume
php artisan queue:work --connection=rabbit-rs
```

## What is Rabbit RS?

Rabbit RS is a native PHP extension written in Rust that provides a high-performance RabbitMQ transport for PHP and Laravel. It uses [Lapin](https://github.com/amqp-rs/lapin) (a Rust AMQP client) behind a testable transport abstraction, and delivers at-least-once semantics with publisher confirms and mandatory routing.

Key features:

- **At-least-once delivery** — publisher confirms and mandatory returns enabled by default
- **Multi-vhost consumption** — a single worker can consume from multiple brokers and vhosts
- **Weighted-fair scheduling** — deficit round-robin with starvation prevention
- **Deterministic recovery** — connection, channels, topology, QoS, then consumers
- **Connection-generation-aware tokens** — stale ACKs are rejected so RabbitMQ redelivers
- **Bounded replay buffer** — unconfirmed publications survive recovery in bounded memory
- **Octane lifecycle** — flush, reload, and stop hooks prevent channel leaks
- **No unsafe Rust** — `#![forbid(unsafe_code)]` across the entire workspace

## Requirements

- **PHP** 8.4 or 8.5
- **Laravel** 12 or 13 (for the Laravel bridge)
- **RabbitMQ** 4.3.x
- **Linux** x86_64 or ARM64 (glibc or musl)
- **Rust** 1.96.0 (contributors only — see [Contributing](#contributing))

## Distribution channels

Rabbit RS is distributed via two channels:

| Package | Channel | Purpose |
|---------|---------|---------|
| `goopil/rabbit-rs-native` | [PIE](https://github.com/php/pie) | Native PHP extension (binary) |
| `goopil/rabbit-rs-laravel` | [Packagist](https://packagist.org) | Laravel queue driver (PHP source) |

PIE selects the correct pre-compiled binary for your PHP version, architecture, libc, and thread-safety mode. Composer installs the Laravel bridge and verifies that `ext-rabbit_rs` is loaded, but does **not** install or modify system PHP binaries.

**Not V1 distribution channels:**

- PECL
- Debian/RPM/APK packages
- Composer plugins that install binaries
- Full PHP images bundling the extension

These are explicitly out of scope for V1. Use PIE to install the extension in your Dockerfile — see [Installation](docs/installation.md).

## Documentation

| Topic | File |
|-------|------|
| Installation | [docs/installation.md](docs/installation.md) |
| Distribution matrix | [docs/distribution.md](docs/distribution.md) |
| Configuration reference | [docs/configuration.md](docs/configuration.md) |
| Laravel usage | [docs/laravel.md](docs/laravel.md) |
| Topology management | [docs/topology.md](docs/topology.md) |
| Reliability and delivery | [docs/reliability.md](docs/reliability.md) |
| Operations | [docs/operations.md](docs/operations.md) |
| Octane integration | [docs/octane.md](docs/octane.md) |
| Troubleshooting | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Development guide | [docs/development.md](docs/development.md) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide. Quick reference:

```bash
# Build the extension
cargo build -p rabbit-rs-php --features extension-tests

# Run tests
./scripts/test-laravel.sh          # Laravel Unit + Feature (no extension)
./scripts/test-extension.sh        # PHP extension (Pest + PHPT)
cargo test -p rabbit-rs-core       # Rust core

# Quality gate
./scripts/check.sh
```

For architecture, build system, test strategy, and common pitfalls, see [docs/development.md](docs/development.md).

## License

MIT. See [LICENSE](LICENSE).
