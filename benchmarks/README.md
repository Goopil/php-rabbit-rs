# Rabbit RS Benchmarks

Standalone PHP benchmark suite for measuring rabbit-rs throughput, latency, and reliability against other PHP RabbitMQ drivers.

## Quick start

### Smoke benchmark

The smoke benchmark runs a single rabbit-rs publish/consume cycle and checks the results against a budget. It requires only the `rabbit_rs` extension and a running RabbitMQ broker.

```bash
php benchmarks/smoke.php
```

Exit code 0 means all budget checks passed; 1 means a regression was detected.

### Comparative benchmark

The comparative benchmark runs multiple drivers (rabbit-rs, php-amqplib, amqp-ext) across safety modes and prints a comparison table.

```bash
composer install --working-dir=benchmarks
php benchmarks/compare.php --driver all --safety all --count 5000 --payload 1024 --batch 64
```

Options:
- `--driver` — `rabbit-rs`, `php-amqplib`, `amqp-ext`, or `all` (default: `all`)
- `--safety` — `unsafe`, `confirms`, `safest`, or `all` (default: `all`)
- `--count` — number of messages (default: `5000`)
- `--payload` — payload size in bytes (default: `1024`)
- `--batch` — batch size (default: `64`)
- `--output` — write JSON results to this path (optional)

## Requirements

- PHP 8.4 or later
- `rabbit_rs` extension loaded (for rabbit-rs driver and smoke benchmark)
- `composer install` in `benchmarks/` for the php-amqplib driver
- pecl `amqp` extension for the amqp-ext driver (optional)
- RabbitMQ broker at `127.0.0.1:5672`

## Safety modes

Drivers adapt their publishing behavior based on the safety parameter:

| Mode | Behavior |
|------|----------|
| `unsafe` | Fire-and-forget. No publisher confirms, no mandatory flag. Fastest but offers no delivery guarantees. |
| `confirms` | Publisher confirms enabled (`confirm_select`). Waits for broker ACK. Detects broker-side losses. |
| `safest` | Publisher confirms + mandatory flag. Detects unroutable messages via basic_return. Slowest but most thorough. |

Note: the rabbit-rs extension always uses confirms internally. For the `unsafe` mode, a very short timeout (100ms) is used to approximate fire-and-forget behavior.

## Budget system

The smoke benchmark checks results against a budget file (`baselines/smoke-budget.json`):

| Metric | Check |
|--------|-------|
| `publish_throughput_min` | `actual >= budget` |
| `consume_throughput_min` | `actual >= budget` |
| `publish_p99_max_ms` | `actual <= budget` |
| `consume_p99_max_ms` | `actual <= budget` |
| `losses_max` | `actual == 0` |

Results are written to `benchmarks/results/smoke-<timestamp>.json`. Comparative results go to `benchmarks/results/compare-<timestamp>.json`.

## Latency measurement

End-to-end publish-to-consume latency is measured by embedding `hrtime(true)` (nanoseconds) in an `x-bench-ts` message header on publish. On consume, the header is read and the delta is computed.

## Directory structure

```
benchmarks/
├── README.md           # This file
├── composer.json       # Package metadata and autoloading
├── smoke.php           # Smoke benchmark script
├── compare.php         # Comparative benchmark script
├── drivers/            # Driver implementations
│   ├── Driver.php
│   ├── RabbitRsDriver.php
│   ├── PhpAmqplibDriver.php
│   └── AmqpExtDriver.php
├── lib/                # Shared utilities
│   ├── Metrics.php
│   └── Budget.php
├── baselines/          # Budget files
│   └── smoke-budget.json
└── results/            # Output directory (gitignored except .gitkeep)
    └── .gitkeep
```
