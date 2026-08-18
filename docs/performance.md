# Rabbit RS Performance Strategy

**Date:** August 18, 2026

---

## Overview

Performance is measured at the PHP level — publish and consume throughput, latency, and resource cost — not at the Rust microbenchmark level. The Rust core is covered by integration tests; benchmarks focus on the user-facing contract.

Two benchmark modes exist:

1. **Smoke** (`benchmarks/smoke.php`) — runs in CI on every push/PR. Publishes and consumes a fixed number of messages through the native extension, measures throughput and p99 latency, checks for zero losses, and compares results against `benchmarks/baselines/smoke-budget.json`. Fails the CI on regression.

2. **Comparative** (`benchmarks/compare.php`) — run manually. Compares `rabbit-rs` (native extension) against `php-amqplib` (pure PHP) and `amqp-ext` (pecl, rabbitmq-c). Tests three safety modes: unsafe (fire-and-forget), confirms-only, and safest (confirms + mandatory). Measures throughput, p50/p95/p99 latency, RSS, and CPU time.

---

## Smoke Budget

The smoke budget (`benchmarks/baselines/smoke-budget.json`) defines anti-regression thresholds:

| Metric | Target |
|--------|--------|
| Publish throughput (256 B, batch 64) | ≥ 5,000 msgs/s |
| Consume throughput (256 B) | ≥ 3,000 msgs/s |
| p99 publish latency (256 B) | ≤ 10 ms |
| p99 consume latency (256 B) | ≤ 50 ms |
| Losses | 0 |

These targets are calibrated on the GitHub Actions ubuntu-latest runner with a single-node RabbitMQ lab. They are intentionally conservative — the goal is to catch regressions, not to measure peak performance.

---

## Running Benchmarks

### Smoke (CI)

```bash
./scripts/lab-up.sh with-plugin
./scripts/lab-ready.sh
php benchmarks/smoke.php
./scripts/lab-down.sh
```

Exit code 0 = pass, 1 = regression detected.

### Comparative (manual)

```bash
cd benchmarks && composer install
./scripts/lab-up.sh with-plugin
./scripts/lab-ready.sh
php benchmarks/compare.php --count 5000 --payload 1024
./scripts/lab-down.sh
```

Options: `--driver rabbit-rs|php-amqplib|amqp-ext|all`, `--safety unsafe|confirms|safest|all`, `--count N`, `--payload BYTES`, `--batch N`.

---

## Methodology

- All benchmarks require a running RabbitMQ broker (`./scripts/lab-up.sh`).
- The smoke benchmark uses a single-node lab (CI-friendly). The comparative benchmark can use the full 3-node cluster.
- Latency is measured as publish-to-consume wall-clock time per message (via `hrtime`).
- Throughput is messages per second over the full publish or consume phase.
- Resource metrics (RSS, CPU) are captured via `getrusage()` and `/proc/self/status` (Linux) or `ps` (macOS).
- Safety modes ensure apples-to-apples comparison: `unsafe` disables confirms and mandatory returns on all drivers; `safest` enables both.
