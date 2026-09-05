# Rabbit RS Performance Strategy

**Date:** August 29, 2026

---

## Overview

Performance is measured at the PHP level — publish and consume throughput, latency, and resource cost — not at the Rust microbenchmark level. The Rust core is covered by integration tests; benchmarks focus on the user-facing contract.

The benchmark suite (`benchmarks/src/run-benchmarks.php`, wrapped by `benchmarks/run-benchmarks.sh`) runs **manually only — no CI workflow executes the runner**. It compares up to four drivers (`rabbit-rs` native extension, `amqplib`, `amqp-ext`, `bunny`) across five scenarios: three transport scenarios (`fire-and-forget`, `batch-confirm`, `auto-ack`) and two Laravel-representative scenarios (`laravel-dispatch`, `laravel-worker`). Each cell measures publish/consume throughput and p50/p95/p99 latency, reports losses and duplicate deliveries, and prints the budget comparison (see below).

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
- Safety is fixed per scenario via `ScenarioMode` (there is no `--safety` flag): `fire-and-forget`, `auto-ack` and `laravel-worker` publish without confirms, while `batch-confirm` and `laravel-dispatch` publish with confirms + mandatory — drivers are compared apples-to-apples within a scenario.

### Soak memory methodology (Round K #143)

`benchmarks/driver-bench/bin/soak.php` doubles as the stability/memory evidence harness:

- **Sampling** (`--sample-interval`, default 10 s): process RSS (`/proc/self/status` on Linux, `ps -o rss=` on macOS), `memory_get_usage(true)`/`memory_get_peak_usage(true)`, and selected `Pool::stats()` counters (`publish_buffered`, drop/reconnect/duplicate totals) are appended to the run's JSON under `memory.samples`. Sampling is O(1) and runs outside the churn loops.
- **Leak detection**: the first 20 % of the run duration is excluded from the fit (allocator warm-up); a least-squares RSS slope is fitted over the post-warmup samples and expressed in MB/hour. The run fails when the slope exceeds `--leak-mb-per-hour` (default 20). A run too short to fit a slope passes without a verdict.
- **Per-cycle tripwire**: the publish buffer must read `publish_buffered == 0` after every cycle's flush; a non-zero reading means publications are parked across cycles (a re-buffer leak path) and fails the run.
- **Modes**: kill mode (`--kill-every=10`, default) proves recovery under churn and requires `reconnects_total >= 1`; steady mode (`--kill-every=0`) is sustained pop+ack with no kills — the cleanest leak signal — and waives the reconnection requirement.
- **Evidence**: long-duration runs (60-min kill, 30-min steady) are archived under `benchmarks/results/round-k-soak/`; the nightly CI soak (#144) archives its own artifacts.
