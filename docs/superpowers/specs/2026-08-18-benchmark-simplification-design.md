# Benchmark Strategy Simplification

**Date:** August 18, 2026
**Status:** Approved
**Supersedes:** `benchmarks/native/` (Criterion microbenchmarks), `benchmarks/laravel/` (comparison lab), `benchmarks/baselines/v1-budget.json`

---

## Problem

The existing benchmark suite grew too detailed. It includes 4 Criterion microbenchmarks (batching, ffi_conversion, scheduler, transport), a PHP FFI harness, and a full Laravel comparison lab with 5 drivers. Despite this complexity, the CI only runs the Criterion benches with minimal samples — the Laravel lab and PHP FFI harness are not in CI at all. The anti-regression budget (`--verify-budget`) is dead code in `run-matrix.sh`. The Laravel comparison targets in `v1-budget.json` are marked `"status": "pending_measurement"`.

What matters is the user-facing performance: publish and consume throughput, latency, and resource cost at the PHP level. The Rust core is already covered by 19 integration test files.

## Decision

Replace the entire `benchmarks/` directory with a simplified PHP standalone benchmark suite:

1. **Smoke benchmark** (`smoke.php`) — runs in CI on every push/PR. Publishes and consumes 2000 messages through the native extension, checks throughput/p99/losses against a JSON budget. Fails CI on regression.

2. **Comparative benchmark** (`compare.php`) — run manually. Compares rabbit-rs (native extension) against php-amqplib (pure PHP) and amqp-ext (pecl, rabbitmq-c) across three safety modes (unsafe, confirms, safest).

## Architecture

```
benchmarks/
├── README.md
├── composer.json              # php-amqplib ^3.7 for comparative benchmark
├── smoke.php                  # CI: publish+consume 2000 msgs, check budget
├── compare.php                # Manual: 3 drivers × 3 safety modes comparison
├── drivers/
│   ├── Driver.php             # Interface + DriverUnavailableException
│   ├── RabbitRsDriver.php      # Native extension (Pool/Consumer)
│   ├── PhpAmqplibDriver.php    # php-amqplib/php-amqplib ^3.7
│   └── AmqpExtDriver.php       # pecl amqp (rabbitmq-c)
├── lib/
│   ├── Metrics.php            # Trait: throughput, p50/p95/p99, RSS, CPU
│   └── Budget.php             # JSON budget loader and checker
├── baselines/
│   └── smoke-budget.json       # Anti-regression thresholds for CI
└── results/
    └── .gitkeep                # Output directory for JSON results
```

### Autoloading

Both `smoke.php` and `compare.php` use a `spl_autoload_register` that maps `Bench\` to `lib/` and `Bench\Drivers\` to `drivers/`. This avoids the composer dependency for `smoke.php` (which only uses the native extension). `compare.php` additionally loads `vendor/autoload.php` if present (for php-amqplib).

### Safety Modes

Drivers adapt their publishing behavior based on the `safety` parameter:

| Mode | Behavior |
|------|----------|
| `unsafe` | Fire-and-forget. No confirms, no mandatory. Fastest. |
| `confirms` | Publisher confirms enabled. Waits for broker ACK. |
| `safest` | Confirms + mandatory flag. Detects unroutable messages. |

The rabbit-rs extension always uses confirms internally. For `unsafe` mode, a 100ms timeout approximates fire-and-forget. For `safest`, the default 30s timeout is used.

### Latency Measurement

End-to-end publish-to-consume latency is measured by embedding `hrtime(true)` (nanoseconds) in an `x-bench-ts` message header on publish. On consume, the header is read and the delta is computed in milliseconds.

### Metrics

All drivers share the `Metrics` trait which provides:
- Throughput: messages/second
- Latency percentiles: p50, p95, p99 (from recorded samples)
- RSS: via `ps` (macOS) or `/proc/self/status` (Linux)
- CPU: via `getrusage()`
- Losses: `expected_count - consumed_count`
- Duplicates: tracked via message_id set (driver-specific)

### Budget System

`smoke-budget.json` defines thresholds:

| Metric | Target |
|--------|--------|
| `publish_throughput_min` | ≥ 5,000 msgs/s |
| `consume_throughput_min` | ≥ 3,000 msgs/s |
| `publish_p99_max_ms` | ≤ 10 ms |
| `consume_p99_max_ms` | ≤ 50 ms |
| `losses_max` | 0 |

The `Budget` class checks publish and consume metrics separately. Each metric is matched by suffix (`_throughput_min` → ≥, `_p99_max_ms` → ≤, `losses_max` → == 0). Exit code 0 on pass, 1 on regression.

## CI Integration

`.github/workflows/bench-smoke.yml` replaces the Criterion bench workflow:

1. Checkout, install Rust 1.96, cache Cargo
2. Setup PHP 8.4, install system deps (jq)
3. Build the extension (`cargo build --release`), install via `./scripts/install.sh --release --yes`
4. Start RabbitMQ lab (`./scripts/lab-up.sh with-plugin`), wait for readiness (120s timeout)
5. Run `php benchmarks/smoke.php`
6. Upload `benchmarks/results/` as artifact (7-day retention)
7. Always stop the lab (`./scripts/lab-down.sh`)

## Files Changed

### Deleted

- `benchmarks/native/` — entire directory (4 Criterion benches + PHP FFI harness)
- `benchmarks/laravel/` — entire directory (5 drivers, Laravel app, run-matrix.sh)
- `benchmarks/baselines/` — entire directory (reference-machine.json, v1-budget.json)
- `benchmarks/BASELINE.md`

### Updated

- `Cargo.toml` — removed `benchmarks/native` from workspace members
- `scripts/check.sh` — removed `--exclude rabbit-rs-native-bench` from cargo test
- `.github/workflows/bench-smoke.yml` — replaced Criterion with PHP smoke benchmark
- `docs/performance.md` — rewritten for the new strategy

### Created

- `benchmarks/smoke.php` — CI smoke benchmark
- `benchmarks/compare.php` — comparative benchmark
- `benchmarks/composer.json` — php-amqplib dependency
- `benchmarks/lib/Metrics.php` — shared metrics trait
- `benchmarks/lib/Budget.php` — budget checker
- `benchmarks/drivers/Driver.php` — driver interface
- `benchmarks/drivers/RabbitRsDriver.php` — native extension driver
- `benchmarks/drivers/PhpAmqplibDriver.php` — php-amqplib driver
- `benchmarks/drivers/AmqpExtDriver.php` — pecl amqp driver
- `benchmarks/baselines/smoke-budget.json` — anti-regression budget
- `benchmarks/README.md` — documentation
- `benchmarks/results/.gitkeep` — output directory

## Verification

- All 8 PHP files pass `php -l` syntax check
- `cargo fmt --check` passes
- `cargo test --workspace --all-targets` — 210 tests pass (20 suites)
- No remaining references to `rabbit-rs-native-bench` outside historical plan docs
