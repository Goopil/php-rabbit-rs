# Rabbit RS Benchmarks

Standalone PHP benchmark suite for measuring rabbit-rs throughput, latency, and reliability against other PHP RabbitMQ drivers.

## Quick start

### Prerequisites

- PHP 8.4 or later
- RabbitMQ broker at `127.0.0.1:5672` (start the lab with `./scripts/lab-up.sh`)
- `rabbit_rs` extension loaded (for rabbit-rs driver)
- `composer install --working-dir=benchmarks` (for amqplib, bunny, and Laravel benchmarks)
- pecl `amqp` extension (for amqp-ext driver, optional)

### Run all benchmarks

```bash
./benchmarks/run-benchmarks.sh
```

This runs all 3 scenarios x all available drivers (up to 12 combinations).

### Run specific driver or scenario

```bash
./benchmarks/run-benchmarks.sh --driver=amqplib
./benchmarks/run-benchmarks.sh --scenario=fire-and-forget
./benchmarks/run-benchmarks.sh --driver=rabbit-rs --scenario=batch-confirm
```

### Release protocol (mandatory)

Comparative numbers are only meaningful under this protocol:

- **Release build mandatory.** Benchmark the extension built in release mode (`./scripts/install.sh --release`); a debug build masks throughput by ~4×.
- **Interleave runs.** When comparing drivers or builds, alternate runs (A/B/A/B) instead of completing one side first, so drift (cache, thermal, broker state) hits both sides equally.
- **Archive one JSON per run.** `results/benchmark-results.json` is overwritten on every run and gitignored — copy the per-run JSON to a durable location (outside the repo) before starting the next run.
- **0 losses / 0 duplicates expected** wherever Safe mode guarantees at-least-once delivery (e.g. `batch-confirm`, `laravel-dispatch`). A non-zero counter invalidates the run — do not record it as a measurement.
- **Broker lab + vhost grant.** Start the lab (`./scripts/lab-up.sh`), wait for readiness (`./scripts/lab-ready.sh`), and make sure the benchmark user (`rabbit_rs`) has permissions on its vhost (`rabbitmqctl set_permissions`).

### Available drivers

| Driver | Requires | Description |
|--------|----------|-------------|
| `rabbit-rs` | ext-rabbit_rs | Native Rust-backed PHP extension |
| `amqplib` | composer install | Pure PHP AMQP client (php-amqplib) |
| `amqp-ext` | pecl amqp | C-based AMQP extension |
| `bunny` | composer install | Async PHP AMQP client (bunny/bunny) |

Drivers are auto-detected based on available extensions and classes.

### Scenarios

| Scenario | Publish | Consume |
|----------|---------|--------|
| `fire-and-forget` | No confirms, no mandatory flag | `no_ack=true` (auto-ack by broker) |
| `batch-confirm` | Batched confirms (every 256 msgs), mandatory flag | Manual ACK |
| `auto-ack` | Per-message confirms, mandatory flag | `no_ack=true` (auto-ack by broker) |

Note: The `rabbit_rs` extension always uses confirms internally. For `fire-and-forget`, a 100ms timeout approximates fire-and-forget behavior.

### Budget system

The smoke budget (`baselines/smoke-budget.json`) checks:

| Metric | Check |
|--------|-------|
| `publish_throughput_min` | `actual >= budget` |
| `consume_throughput_min` | `actual >= budget` |
| `publish_p99_max_ms` | `actual <= budget` |
| `consume_p99_max_ms` | `actual <= budget` |
| `losses_max` | `actual == 0` |

### Configuration

All benchmark parameters are in `src/Config.php`:
- 10,000 messages per round, 10 rounds (+ 1 warmup)
- 256-byte payload
- RabbitMQ: `127.0.0.1:5672`, user `rabbit_rs`, vhost `/`

### Latency measurement

End-to-end publish-to-consume latency is measured by embedding `hrtime(true)` (nanoseconds) in the first 8 bytes of the message payload (packed as 64-bit unsigned int). On consume, the timestamp is unpacked and the delta is computed.

### Output

Results are printed to stdout and written to `results/benchmark-results.json`.

### Directory structure

```
benchmarks/
├── README.md
├── composer.json
├── docker-compose.yml       # Standalone RabbitMQ (for CI, uses rabbit_rs/rabbit_rs_lab)
├── run-benchmarks.sh         # Shell wrapper
├── baselines/
│   └── smoke-budget.json     # Budget thresholds
├── results/                  # Output directory (gitignored)
├── src/
│   ├── run-benchmarks.php    # Main runner
│   ├── AbstractBenchmark.php # Base class: measurement loop, stats
│   ├── Config.php            # Static config constants
│   ├── ScenarioMode.php       # Scenario mode constants
│   ├── Budget.php             # Budget checking
│   ├── Drivers/
│   │   ├── AmqplibDriver.php
│   │   ├── AmqpExtDriver.php
│   │   ├── BunnyDriver.php
│   │   └── RabbitRsDriver.php
│   └── Scenarios/
│       ├── FireAndForgetBenchmark.php
│       ├── BatchConfirmBenchmark.php
│       └── AutoAckBenchmark.php
└── laravel/
    ├── LaravelCompareBenchmark.php
    └── LaravelSmokeBenchmark.php
```

### Rust microbenchmarks

Criterion benchmarks for subsystem-level performance (if available):

```bash
cargo bench -p rabbit-rs-core
```
