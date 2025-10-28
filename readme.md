# 🐇 RabbitRs — PHP AMQP Extension Powered by Rust

This library is in it's early stage and need some more care before production usage. 

RabbitRs is a native PHP extension implemented in Rust with
[`ext-php-rs`](https://github.com/davidcole1340/ext-php-rs). It provides a fast,
memory-safe AMQP 0-9-1 client built on top of
[`lapin`](https://crates.io/crates/lapin), and is designed as a drop-in upgrade
for [`php-amqplib`](https://github.com/php-amqplib/php-amqplib).

RabbitRs was born from running RabbitMQ at scale and fighting connection storms.
The project focuses on reusing connections, delivering predictable throughput,
and giving PHP developers the ergonomics they expect from `php-amqplib` while
leveraging the performance and safety of Rust.

---

## ✨ Highlights

- **Zero PHP FFI** – native Zend extension compiled from Rust.
- **Shared connection model** – one TCP/TLS connection per PHP worker, reused by
  every `PhpClient` in the same process.
- **Publisher confirms** – confirm mode enabled by default with blocking
  confirms, plus compatibility helpers (`confirmSelect`, `waitForConfirms`).
- **Manual or automatic acknowledgements** – full control over
  `ack`/`nack`/`reject`, matching `php-amqplib` semantics.
- **Consumer QoS** – manual QoS or adaptive auto-tuning that grows/shrinks the
  prefetch based on observed throughput.
- **TLS & reconnection** – native TLS via `rustls` with configurable
  reconnect/backoff policies.
- **Full message metadata** – headers, properties, and binary payloads flow both
  ways without lossy conversions.

---

## 🧱 Architecture Overview

| Layer            | Implementation details                                                                              |
|------------------|-----------------------------------------------------------------------------------------------------|
| PHP API          | Classes generated with `ext-php-rs` (`Goopil\RabbitRs\PhpClient`, `Goopil\RabbitRs\PhpChannel`, …). |
| Core runtime     | Asynchronous orchestration with `tokio`, message pipelines on top of `lapin`.                       |
| Connection model | `once_cell` + `parking_lot` locks coordinate a single shared connection per process.                |
| Backpressure     | Adaptive QoS uses moving windows and cooldowns to resize prefetch without thundering.               |
| Observability    | `tracing` instrumentation is available when compiled with the `tracing` feature flag.               |

---

## 🚀 Getting Started

### Prerequisites

- Rust 1.89+
- PHP 8.2 – 8.5 (CLI, FPM, or Apache module)
- RabbitMQ (local or remote) for integration tests

### Build the extension

```bash
cargo build --release
```

The compiled module is available in `target/release/librabbit_rs.(so|dylib|dll)`
depending on your platform.

### Enable the extension

Add the module to your `php.ini` (or drop a `.ini` in `conf.d`):

```ini
extension=/path/to/target/release/librabbit_rs.so
```

Verify the load:

```bash
php -m | grep rabbit_rs
```

---

## 📦 Composer Installer Plugin

A Composer plugin is provided under `php/composer-plugin`. It:

1. Detects the current platform/architecture (`linux-gnu`, `linux-musl`,
   `darwin`, `windows` + `x86_64`/`aarch64`).
2. Downloads the matching prebuilt binary from GitHub Releases.
3. Caches the module under `vendor/rabbit-rs/ext/<platform>/php<version>`.
4. Publishes `RabbitRs.stub.php` for IDE autocompletion.

Minimal configuration snippet:

```json
{
  "require": {
    "goopil/rabbit-rs-installer": "^0.1"
  },
  "extra": {
    "rabbit-rs": {
      "version": "0.1.0",
      "download_template": "https://github.com/goopil/php-rabbit-rs/releases/download/%tag%/%file%"
    }
  }
}
```

Set `RABBIT_RS_VERSION=vX.Y.Z` to override the release tag or
`RABBIT_RS_FORCE_INSTALL=1` to force a re-download.

---

## 💡 Usage Samples

### Basic connection

```php
use Goopil\RabbitRs\PhpClient;

$client = new PhpClient([
    'host' => 'localhost',
    'user' => 'guest',
    'password' => 'guest',
]);

$client->connect();          // idempotent
$channel = $client->openChannel();
$channel->queueDeclare('jobs', ['durable' => true]);
$channel->basicPublish('', 'jobs', new Goopil\RabbitRs\AmqpMessage('hello'));
```

### TLS connection

```php
$client = new PhpClient([
    'host' => 'my-rabbit',
    'port' => 5671,
    'user' => 'guest',
    'password' => 'guest',
    'ssl' => [
        'cafile' => '/path/ca.pem',
        'certfile' => '/path/client.pem',
        'keyfile' => '/path/client.key',
    ],
]);

$client->connect();
```

### Reconnect with exponential backoff

```php
$client = new PhpClient([
    'host' => 'cluster',
    'user' => 'guest',
    'password' => 'guest',
    'reconnect' => [
        'enabled' => true,
        'max_retries' => 5,
        'initial_delay_ms' => 100,
        'max_delay_ms' => 5000,
        'jitter' => 0.2,
    ],
]);

$client->connect();
```

---

## 🧪 Testing

- **Rust unit tests**: `cargo test --all`
- **PHP integration suite**: `bash run-test.sh`  
  Spins up RabbitMQ with Docker Compose, builds the extension (debug or release),
  and runs Pest tests from `php/tests/`.
- **Benchmarks**: `bash run-benchmarks.sh`  
  Launches a dedicated RabbitMQ instance, ensures benchmark dependencies are
  installed, and executes the comparison suite under `php/benchmarks/`.

The GitHub Actions CI workflow executes formatting checks (`cargo fmt`),
`clippy`, Rust tests, and the integration suite on PHP 8.2.

---

## 🛠️ Release & Distribution

Tagged releases trigger `.github/workflows/release.yml`, which:

1. Builds Linux glibc binaries directly on Ubuntu runners.
2. Builds Linux musl binaries with dedicated Docker builders (`docker/linux-*`).
3. Compiles macOS (x86_64 & arm64) using `shivammathur/setup-php`.
4. Packages artifacts via `scripts/package-extension.sh`, producing `.zip` files,
   `.ini` snippets, and SHA256 sums.
5. Publishes everything to the GitHub Release along with an aggregated checksum
   manifest.

Future work includes native Linux packages (`.deb`, `.rpm`) and Windows builds.

---

## 🗺️ Roadmap

### 🎯 MVP (first public release)

- [x] Single shared connection per process (no pool, no redundant connections).
- [x] Graceful shutdown at `MSHUTDOWN` (PHP-FPM, CLI, Apache).
- [x] Reconnect options (enabled, retries, delays, jitter).
- [x] Queue/Exchange declare & bind helpers aligned with `php-amqplib`.
- [x] Basic publish with confirms.
- [x] Basic consume with callback.
- [x] `ack`/`nack`/`reject` parity.
- [ ] Surface `connection_name` (host:pid) in RabbitMQ management UI.
- [x] Unit tests + Pest integration suite (connection, publish, declare, shutdown).
- [ ] Improved error propagation (always throw `PhpException` for protocol errors).
- [x] Benchmark harness (publish/consume throughput & latency).
- [x] Automated release pipeline (PHP 8.2–8.5rc, Ubuntu/Alpine/macOS).
- [ ] Transaction support (`txSelect`, `txCommit`, `txRollback`).  
- [ ] Return Message listener
- [ ] Improve consume performance

### 🚀 Next Milestone

- [x] Full FieldTable support (args, headers, nested arrays).
- [x] Symmetric header propagation.
- [ ] Async iterator-based consumer API.
- [ ] Lazy channel recreation after reconnect.
- [ ] Consumer recovery with auto QoS reset.
- [ ] Configurable benchmark scenarios (message size, durability, QoS).
- [ ] Release documentation (semver policy, changelog template).
- [x] Composer installer + IDE stubs.

### 🔮 Future enhancements

- [ ] JSON convenience APIs (`basicPublishJson`).
- [x] Adaptive QoS for consumers.
- [ ] Buffer reuse to cut allocations.
- [x] Graceful SIGTERM drain strategy.
- [ ] Best-practice guide (connection reuse, channel fan-out).
- [x] Performance reports (RabbitRs vs `php-amqplib` vs `bunny`).
- [ ] Prebuilt native packages (deb/rpm) and Windows distribution.
- [ ] Framework integrations (Laravel, Symfony, etc.).
- [ ] Multi-channel consumer.

---

## 🤝 Contributing

1. Fork and clone the repository.
2. Install Rust & PHP build dependencies.
3. Run `cargo fmt`, `cargo clippy`, and `bash run-test.sh` before submitting a PR.
4. Describe testing performed in the pull request template.

Bug reports and performance traces are very welcome—production feedback drives
our prioritisation.

---

## 📄 License

RabbitRs is released under the MIT License. See `LICENSE` for details.
