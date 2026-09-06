# Rabbit RS

> **Native Rust RabbitMQ transport for high-throughput, long-running PHP/Laravel workers.**

[![CI](https://github.com/Goopil/php-rabbit-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Goopil/php-rabbit-rs/actions/workflows/ci.yml)
[![Release](https://github.com/Goopil/php-rabbit-rs/actions/workflows/release.yml/badge.svg)](https://github.com/Goopil/php-rabbit-rs/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

Rabbit RS is a PHP extension written in Rust. It moves the connection pool, publisher confirms, consumer scheduling, and connection recovery out of PHP userspace, behind the standard Laravel queue API.

The product is built around one feature: the **long-running consumer** — a worker that multiplexes many queues per broker connection, survives broker restarts and network failures, and keeps settling deliveries through recovery without manual intervention. A supervised fan-out command runs one worker per connection to span vhosts and brokers.

Delivery is **at-least-once**: silent loss is unacceptable; duplicates are permitted and must remain measurable.

## What it does

- **Long-running consumer** — one worker multiplexes subscriptions across many queues on its connection and survives broker restarts without manual intervention
- **At-least-once delivery** — publisher confirms and mandatory routing enabled by default; every publish is tracked to ACK, return, or timeout
- **Deterministic recovery** — connection, channels, exchanges, queues, bindings, QoS, then consumers, in a fixed order
- **Weighted-fair scheduling** — deficit round-robin across subscriptions with starvation prevention
- **Connection-generation-aware tokens** — stale ACKs are rejected so RabbitMQ redelivers
- **Bounded replay buffer** — unconfirmed publications survive connection recovery in bounded memory, replayed with the same `message_id`
- **Multi-broker fan-out** — a vhost owns a distinct AMQP connection; define one connection per broker/vhost and `rabbit-rs:work` supervises them all
- **Octane lifecycle** — flush, reload, and stop hooks prevent channel leaks
- **No unsafe Rust** — `#![forbid(unsafe_code)]` across the entire workspace

> On the curated lab workloads, rabbit-rs consumes **4–6× faster** than php-amqplib on the same session, with 0 losses and 0 duplicates in every reliable-mode run. Harness, methodology, and archived results: [benchmarks/README.md](benchmarks/README.md).

## Quick start

**Step 1 — Install the native extension:**

```bash
pie install goopil/rabbit-rs-native
```

**Step 2 — Install the Laravel queue driver:**

```bash
composer require goopil/rabbit-rs-laravel
```

**Step 3 — Configure the connection:**

Add a rabbit-rs connection to `config/queue.php` (one connection = one broker = one native pool):

```php
'connections' => [
    'rabbit-rs' => [
        'driver' => 'rabbit-rs',
        'queue' => env('RABBIT_RS_QUEUE', 'default'),
        'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
        'username' => env('RABBIT_RS_USERNAME', 'guest'),
        'password' => env('RABBIT_RS_PASSWORD', 'guest'),
    ],
],
```

Configuration is connection-first — broker, credentials, routes, safety mode, and worker profile all live on the queue connection. The full reference (every key, defaults, validation, and the safety modes) is [docs/configuration.md](docs/configuration.md). Optionally publish the cross-cutting defaults:

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
php artisan queue:work rabbit-rs
# or the supervised fan-out across every rabbit-rs connection:
php artisan rabbit-rs:work
```

## Requirements

- **PHP** 8.4 or 8.5
- **Laravel** 12 or 13 (for the Laravel queue driver)
- **RabbitMQ** 4.2.9 or newer (the CI lab runs 4.2.9)
- **Linux** x86_64 or ARM64 (glibc or musl) — pre-compiled binaries via PIE
- **macOS** ARM64 (Apple Silicon) — pre-compiled binary from [GitHub Releases](https://github.com/Goopil/php-rabbit-rs/releases)
- **Rust** 1.96.0 (contributors only — see [Contributing](#contributing))

## Distribution channels

Rabbit RS is distributed via three channels:

| Package | Channel | Purpose |
|---------|---------|---------|
| `goopil/rabbit-rs-native` | [PIE](https://github.com/php/pie) | Native PHP extension (Linux binary) |
| `goopil/rabbit-rs-laravel` | [Packagist](https://packagist.org) | Laravel queue driver (PHP source) |
| `rabbit-rs` | [Homebrew](https://github.com/Goopil/homebrew-rabbit-rs) | Native PHP extension (macOS binary) |

PIE selects the correct pre-compiled binary for your PHP version, architecture, libc, and thread-safety mode. Homebrew does the same for macOS Apple Silicon. Composer installs the Laravel bridge and verifies that `ext-rabbit_rs` is loaded, but does **not** install or modify system PHP binaries.

### macOS

**Homebrew (Apple Silicon):**

```bash
brew tap goopil/rabbit-rs
brew install rabbit-rs
```

Requires PHP 8.4 or 8.5 installed via Homebrew.

**Manual install (Apple Silicon):**

PIE does not support macOS. On Apple Silicon (ARM64), download the pre-compiled binary from [GitHub Releases](https://github.com/Goopil/php-rabbit-rs/releases) and load it manually:

```bash
# Download the matching asset for your PHP version
unzip php_rabbit_rs-*_php8.4-arm64-darwin-nts.zip
cp rabbit_rs.so $(php-config --extension-dir)/rabbit_rs.so
echo "extension=rabbit_rs" > $(php-config --ini-dir)/ext-rabbit_rs.ini
php -m | grep rabbit_rs
```

Alternatively, build from source:

```bash
git clone https://github.com/Goopil/php-rabbit-rs.git
cd php-rabbit-rs
./scripts/install.sh --release
```

Intel Macs (x86_64) are not distributed as pre-compiled binaries — build from source with `./scripts/install.sh --release`.

**Not V1 distribution channels:**

- PECL
- Debian/RPM/APK packages
- Composer plugins that install binaries
- Full PHP images bundling the extension

These are explicitly out of scope for V1. Use PIE to install the extension in your Dockerfile — see [Installation](docs/installation.md).

## Limitations

Read before betting a pipeline on this.

1. **At-least-once means duplicates are possible — by contract, and measured.** The broker redelivers any delivery that is not acknowledged (worker crash, channel loss, recovery), so consumers must treat an extra copy as normal: **jobs must be idempotent**. Duplicates are counted per run and become possible after any reconnect. The counters (`duplicates_total`, `messages_redelivered`) and idempotency guidance live in [docs/reliability.md](docs/reliability.md).
2. **The replay buffer is in-process memory, not durability.** Unconfirmed publications survive *connection* recovery in a bounded in-process buffer (1024 publications / 64 MiB by default) and are replayed with the same `message_id` and original deadline. A **PHP process crash empties it**: the broker may never have received those messages, and nothing redelivers them to you. Broker redelivery after a crash covers the consume side (duplicates, not loss) — it is not publish durability. For cross-process durability you need an **external outbox**, which Rabbit RS does not include ([docs/reliability.md](docs/reliability.md#what-the-replay-buffer-is-not)).

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
| Performance strategy | [docs/performance.md](docs/performance.md) |
| Octane integration | [docs/octane.md](docs/octane.md) |
| Troubleshooting | [docs/troubleshooting.md](docs/troubleshooting.md) |
| Benchmark harness and archived results | [benchmarks/README.md](benchmarks/README.md) |
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
