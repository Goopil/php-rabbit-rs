# Rabbit RS

> **Native Rust RabbitMQ transport for high-throughput, long-running PHP/Laravel workers.**

[![CI](https://github.com/Goopil/php-rabbit-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Goopil/php-rabbit-rs/actions/workflows/ci.yml)
[![Release](https://github.com/Goopil/php-rabbit-rs/actions/workflows/release.yml/badge.svg)](https://github.com/Goopil/php-rabbit-rs/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)

Rabbit RS is a PHP extension written in Rust. It moves the connection pool, publisher confirms, consumer scheduling, and connection recovery out of PHP userspace, behind the standard Laravel queue API.

Delivery is **at-least-once**: silent loss is unacceptable; duplicates are permitted and must remain measurable. The product is built around one feature: the **long-running consumer** — a single worker that consumes from many queues, vhosts, and brokers, survives broker restarts and network failures, and keeps settling deliveries through recovery without manual intervention.

## Benchmarks

Every number in this README comes from one of two curated fresh-lab archives; each claim names the archive and the workload it was measured on. No absolute claims — the phrasing is always "on this workload, with this configuration and these guarantees, X against Y".

| Archive | Date | Extension build | Harness |
|---------|------|-----------------|---------|
| [`benchmarks/results/round-i-rebench`](benchmarks/results/round-i-rebench/README.md) | 2026-09-03 | rabbit_rs 0.0.9, release | driver-level + transport-level |
| [`benchmarks/results/round-d-safe-flush`](benchmarks/results/round-d-safe-flush/README.md) | 2026-09-04 | rabbit_rs 0.1.0, release | driver-level (safety ladder) |

Common workload for both archives: fresh 3-node RabbitMQ 4.2.9 cluster on localhost, PHP 8.5.6 (cli), Laravel 13.29, macOS (Apple Silicon), release-build cdylib loaded per-run. Driver-level: framework queue API, 1024 B Laravel envelope, 1000 ops × 10 rounds × 3 runs, medians over 30 measured rounds. Transport-level: 10 000 msgs/round × 10 rounds + warmup, medians over 3 interleaved invocations. **Every reliable-mode run in both archives measured 0 losses and 0 duplicates.** Reproduce with the commands in each archive.

### The long-running worker

On the driver-level worker cell — one `pop()` + one `ack()` per job through the framework queue API — rabbit-rs processes **16 234 ops/s** against **2 029 ops/s** for the amqplib-based reference driver: **8.0× on the same workload, same session** ([round-i-rebench](benchmarks/results/round-i-rebench/README.md)).

In the same harness and session, fire-and-forget framework dispatch (`blind`) reaches **21 992 ops/s** against 9 685 for the reference dispatch cell — 2.3× ([round-i-rebench](benchmarks/results/round-i-rebench/README.md)).

### Transport-level: consume lead, publish parity

php-amqplib runs the identical scenario semantics on the same cluster. Medians ([round-i-rebench](benchmarks/results/round-i-rebench/README.md)):

| Scenario | rabbit-rs publish | amqplib publish | rabbit-rs consume | amqplib consume | consume lead |
|----------|------------------:|----------------:|------------------:|----------------:|--------------|
| fire-and-forget (256 B payload) ¹ | 219 713 msg/s | 40 773 msg/s | 15 703 msg/s | 13 220 msg/s | 1.2× |
| batch-confirm (256 B payload) ² | 31 813 msg/s | 30 968 msg/s | 40 768 msg/s | 9 489 msg/s | **4.3×** |
| laravel-dispatch (1024 B payload) ³ | 15 255 msg/s | 20 258 msg/s | 38 370 msg/s | 9 694 msg/s | **4.0×** |
| laravel-worker (1024 B payload) ⁴ | 191 913 msg/s ⁵ | 38 643 msg/s | 12 131 msg/s | 1 896 msg/s | **6.4×** |

¹ no confirms, broker auto-ack; rabbit-rs publishes in native `blind` mode. ² batched confirms (every 256 msgs) + mandatory; manual ack. ³ unitary confirm + mandatory per message (`safe`). ⁴ unitary consume + ack per message, prefetch 64. ⁵ publish is a fast batch fill in `blind` mode — not the headline metric; the worker cell's headline is consume.

Read: the consumer lead on the manual-ack scenarios is **4.0–6.4×**; confirm-bound publish **matches amqplib** (batch-confirm 1.03×); the only cell where rabbit-rs trails in the same-session comparison is `laravel-dispatch` publish — one confirm + mandatory round-trip per message at 0.75× amqplib — the documented optimization target ([round-i-rebench](benchmarks/results/round-i-rebench/README.md)).

Cross-session absolute deltas are a session factor — the archives keep an unchanged third-party driver (vladimir, pure PHP) as a control for exactly this reason: its dispatch cell dropped −70% between sessions while its code was unchanged — so same-session ratios, not cross-session deltas, are quoted here.

### The safety ladder: a trade-off, not a cliff

| Mode | Guarantee |
|------|-----------|
| `safe` (default) | publisher confirms + mandatory routing — at-least-once, every outcome surfaced |
| `unsafe` | confirms without mandatory — unroutable messages are silently dropped by the broker |
| `blind` | explicit fire-and-forget into a bounded background pump — a transport failure after hand-off is a silent loss; delayed jobs are not honored |

On the driver-level dispatch cell (unit framework publishes, 1024 B envelope, medians over 30 measured rounds, [round-d-safe-flush](benchmarks/results/round-d-safe-flush/README.md)):

| Mode | Before Round D | After Round D (current) |
|------|---------------:|------------------------:|
| `safe` | 5 729 ops/s | **20 866 ops/s** (×3.64) |
| `blind` | 21 654 ops/s | 21 939 ops/s |

A same-session probe of the full ladder before the fix measured blind 21 036 → unsafe 17 782 → safe 5 445 ops/s ([round-d-safe-flush](benchmarks/results/round-d-safe-flush/README.md), Phase 1). After Round D's pipelined flush (#41), **`safe` reaches 0.95× of `blind`** on this workload — both cells sit at the produce ceiling. Keeping confirms + mandatory costs ~5% on this cell; dropping to `blind` buys that ~5% and gives up outcome tracking, mandatory returns, and delay routing. The ladder is a semantics trade-off, not a throughput cliff.

## Limitations

Read before betting a pipeline on this.

1. **At-least-once means duplicates are possible — by contract, and measured.** The broker redelivers any delivery that is not acknowledged (worker crash, channel loss, recovery), so consumers must treat an extra copy as normal: **jobs must be idempotent**. Duplicates are counted per run — 0 in every reliable-mode run of both archives — and become possible after any reconnect. The counters (`duplicates_total`, `messages_redelivered`) and idempotency guidance live in [docs/reliability.md](docs/reliability.md).
2. **The replay buffer is in-process memory, not durability.** Unconfirmed publications survive *connection* recovery in a bounded in-process buffer (1024 publications / 64 MiB by default) and are replayed with the same `message_id` and original deadline. A **PHP process crash empties it**: the broker may never have received those messages, and nothing redelivers them to you. Broker redelivery after a crash covers the consume side (duplicates, not loss) — it is not publish durability. For cross-process durability you need an **external outbox**, which Rabbit RS does not include ([docs/reliability.md](docs/reliability.md#what-the-replay-buffer-is-not)).
3. **Publisher confirms were the publish throughput ceiling — resolved by pipelining (Round D, #41).** Before Round D, each flush batch serialized its confirm-wave against production (the `block_on` barrier was 74% of wall time on the lab): `safe` measured 5 729 ops/s against 21 654 `blind` on the dispatch cell. The pipelined flush (#41) drains the buffer on the runtime and returns before confirmations resolve, keeping every ceiling (bounded buffer, in-flight window) and every outcome: 20 866 ops/s, 0.95× `blind`, contract held across 120 measured rounds with 0 losses ([round-d-safe-flush](benchmarks/results/round-d-safe-flush/README.md)). The remaining known publish gap is the `laravel-dispatch` transport cell (unitary confirm + mandatory per message): 15 255 msg/s, 0.75× amqplib on the same session ([round-i-rebench](benchmarks/results/round-i-rebench/README.md)).

## Quick start

**Step 1 — Install the native extension:**

```bash
pie install goopil/rabbit-rs-native
```

**Step 2 — Install the Laravel bridge:**

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

Configuration is connection-first — broker, credentials, routes, safety mode, and consumer profile all live on the queue connection. The full reference (every key, defaults, validation, and the safety modes) is [docs/configuration.md](docs/configuration.md). Optionally publish the cross-cutting defaults:

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
# or the supervised fan-out across every rabbit-rs connection:
php artisan rabbit-rs:work
```

## What is Rabbit RS?

Rabbit RS is a native PHP extension written in Rust that provides a high-performance RabbitMQ transport for PHP and Laravel. It uses [Lapin](https://github.com/amqp-rs/lapin) (a Rust AMQP client) behind a testable transport abstraction, and delivers at-least-once semantics with publisher confirms and mandatory routing.

Key features:

- **Long-running consumer** — one worker multiplexes subscriptions across queues, vhosts, and brokers, and survives broker restarts without manual intervention
- **Weighted-fair scheduling** — deficit round-robin across subscriptions with starvation prevention
- **Deterministic recovery** — connection, channels, topology, QoS, then consumers
- **At-least-once delivery** — publisher confirms and mandatory returns enabled by default
- **Connection-generation-aware tokens** — stale ACKs are rejected so RabbitMQ redelivers
- **Bounded replay buffer** — unconfirmed publications survive connection recovery in bounded memory
- **Multi-vhost consumption** — a single worker can consume from multiple brokers and vhosts
- **Octane lifecycle** — flush, reload, and stop hooks prevent channel leaks
- **No unsafe Rust** — `#![forbid(unsafe_code)]` across the entire workspace

## Requirements

- **PHP** 8.4 or 8.5
- **Laravel** 12 or 13 (for the Laravel bridge)
- **RabbitMQ** 4.3.x
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
