# Rabbit RS Performance Strategy

**Date:** August 29, 2026

---

## Overview

Performance is measured at the PHP level — publish and consume throughput, latency, and resource cost — not at the Rust microbenchmark level. The Rust core is covered by integration tests; benchmarks focus on the user-facing contract.

The benchmark suite (`benchmarks/run-benchmarks.php`, wrapped by `benchmarks/run-benchmarks.sh`) runs **manually only — no CI workflow executes the runner**. It compares up to four drivers (`rabbit-rs` native extension, `amqplib`, `amqp-ext`, `bunny`) across five scenarios: three transport scenarios (`fire-and-forget`, `batch-confirm`, `auto-ack`) and two Laravel-representative scenarios (`laravel-dispatch`, `laravel-worker`). Each cell measures publish/consume throughput and p50/p95/p99 latency, reports losses and duplicate deliveries, and prints the budget comparison (see below).

**Release runs must follow the [release protocol](../benchmarks/README.md#release-protocol-mandatory)** (release build, interleaved runs, per-run JSON archived, 0 losses/0 duplicates expected in Safe).

---

## Smoke Budget

The smoke budget (`benchmarks/baselines/smoke-budget.json`) defines rough sanity thresholds:

| Metric | Threshold |
|--------|-----------|
| Publish throughput | ≥ 1,000 msgs/s |
| Consume throughput | ≥ 500 msgs/s |
| p99 publish latency | ≤ 2,000 ms |
| p99 consume latency | ≤ 2,000 ms |
| Losses | 0 |

**No CI runs the benchmark runner** — `run-benchmarks.php` prints the budget comparison but does not fail anything, so the budgets are informational. Treat them as a manual smoke signal on your own hardware, not an anti-regression gate.

---

## Running Benchmarks

### Benchmark suite (manual)

```bash
./scripts/lab-up.sh with-plugin
./scripts/lab-ready.sh
./benchmarks/run-benchmarks.sh
./scripts/lab-down.sh
```

Options: `--driver=rabbit-rs|amqplib|amqp-ext|bunny`, `--scenario=fire-and-forget|batch-confirm|auto-ack|laravel-dispatch|laravel-worker`. Drivers are auto-detected (a missing extension or composer dependency skips its driver).

---

## Methodology

- All benchmarks require a running RabbitMQ broker (`./scripts/lab-up.sh`).
- Latency is measured as publish-to-consume wall-clock time per message (via `hrtime`).
- Throughput is messages per second over the full publish or consume phase.
- Resource metrics (RSS, CPU) are captured via `getrusage()` and `/proc/self/status` (Linux) or `ps` (macOS).
- Safety modes ensure apples-to-apples comparison: `unsafe` disables confirms and mandatory returns on all drivers; `safest` enables both.
