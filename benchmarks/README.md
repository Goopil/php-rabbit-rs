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

This runs all 5 scenarios x all available drivers (up to 20 combinations).

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
- **Uninstall the release build after the runs.** Run `cargo php remove --manifest crates/rabbit-rs-php/Cargo.toml --yes` and delete any `ext-rabbit_rs.ini` in the PHP conf.d directory — the Laravel Unit/Feature suite asserts the extension is absent (`RabbitMqServiceProviderTest`) and fails while it stays installed.

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
| `laravel-dispatch` | Unit publishes, confirms + mandatory (Safe), 1024 B payload | Fast batch drain (not the headline metric) |
| `laravel-worker` | Fast batch fill, blind (not the headline metric) | Unit consume + ACK per message, 1024 B payload, prefetch 64 |

Note: for the `rabbit-rs` driver, the no-confirm scenarios (`fire-and-forget`, `auto-ack`, `laravel-worker`) run in the native `blind` safety mode.

### Budget system

Two layers with different contracts:

#### RC budget gate (anti-regression, blocking)

`baselines/reference-machine.json` stores the measured baseline for the reference
machine (real machine spec, PHP/driver versions, date, per-cell numbers); the
checker `baselines/check-budgets.php` compares a result run — one JSON or a
directory of them — against it. The RC pipeline runs this checker as a blocking
gate (wired by the RC-pipeline workstream); nightly benchmark runs are optional.

| Metric | Budget |
|--------|--------|
| publish throughput (dispatch cells) | ≥ 80 % of baseline |
| consume throughput (worker cell) | ≥ 80 % of baseline |
| publish / consume per-op p99 | ≤ 150 % of baseline |
| run integrity (`ok`), losses/`missing`, duplicates, final `publish_buffered`, buffered tripwire | **always blocking** — any non-zero / `false` fails |

The ratio metrics are compared per scenario as the **median of the runs in the
invocation**: one noise-contaminated run does not move the median, while a real
regression shifts every run. Rows marked `n/a` mean "not measured" or "no
baseline for this scenario" and never count as passing; a run where no check
could be applied fails loudly. Exit codes: `0` pass, `1` budget fail or
unjudgeable data, `2` usage error.

```bash
php benchmarks/baselines/check-budgets.php benchmarks/results/ws8-baseline-runs
```

#### Smoke budget (manual signal)

`baselines/smoke-budget.json` holds absolute floors (publish ≥ 1,000 msgs/s,
consume ≥ 500 msgs/s, p99 ≤ 2,000 ms publish and consume, losses == 0) that
`run-benchmarks.php` prints a comparison for. **No CI runs that runner** — the
smoke budget stays a manual signal on your own hardware, not a gate.

#### Re-baselining

The baseline is machine-specific: it records exactly which machine, versions and
cells it covers. Re-baseline when the reference environment changes (hardware,
macOS/OS, Docker/broker version, PHP or driver major versions) or after an
intentional, reviewed performance trade-off — never to silence a failing gate.
Procedure and policy: `docs/performance.md`.

### Soak memory methodology (Round K #143)

`benchmarks/driver-bench/bin/soak.php` doubles as the stability/memory evidence harness:

- **Sampling** (`--sample-interval`, default 10 s): process RSS (`/proc/self/status` on Linux, `ps -o rss=` on macOS), `memory_get_usage(true)`/`memory_get_peak_usage(true)`, and selected `Pool::stats()` counters (`publish_buffered`, drop/reconnect/duplicate totals) are appended to the run's JSON under `memory.samples`. Sampling is O(1) and runs outside the churn loops. Sequence bookkeeping uses a compact bitset, so harness memory stays flat and cannot contaminate the signal.
- **Leak detection**: the first 20 % of the run duration is excluded from the fit (allocator warm-up). The fit is a least-squares slope over the **peak envelope seeded by the warmup peak** (expressed in MB/hour): under kill churn the allocator oscillates between bounded states with deep transient dips (a raw fit misreads uneven plateau durations as growth — measured on the kill-60min calibration run), while a genuine leak must exceed the warmup peak and keep climbing, raising the envelope. The run fails when the slope exceeds `--leak-mb-per-hour` (default 20). A run too short to fit a slope passes without a verdict.
- **Per-cycle tripwire**: the publish buffer must read `publish_buffered == 0` after every cycle's flush; a non-zero reading means publications are parked across cycles (a re-buffer leak path) and fails the run.
- **Modes**: kill mode (`--kill-every=10`, default) proves recovery under churn and requires `reconnects_total >= 1`; steady mode (`--kill-every=0`) is sustained pop+ack with no kills — the cleanest leak signal — and waives the reconnection requirement.
- **Evidence**: long-duration runs (60-min kill, 30-min steady) are archived under `benchmarks/results/round-k-soak/`; the nightly CI soak (#144) archives its own artifacts.

### Configuration

All benchmark parameters are in `src/Config.php`:
- 10,000 messages per round, 10 rounds (+ 1 warmup)
- 256 B payload (`Config::MESSAGE_PAYLOAD_BYTES`) — 1024 B for the `laravel-*` scenarios (`Config::MESSAGE_PAYLOAD_LARAVEL_BYTES`)
- RabbitMQ: `127.0.0.1:5672`, user `rabbit_rs`, vhost `/`

### Latency measurement

End-to-end publish-to-consume latency is measured by embedding `hrtime(true)` (nanoseconds) in the first 8 bytes of the message payload (packed as 64-bit unsigned int). On consume, the timestamp is unpacked and the delta is computed.

### Output

Results are printed to stdout and written to `results/benchmark-results.json`.

### Published result metric contract (Round J, #127)

Every published result set — the transport suite (this `results/benchmark-results.json`, copied per run into an archive), the driver-level suite (`driver-bench/bin/bench.php`), and the soak harness (`driver-bench/bin/soak.php`) — surfaces the full metric set next to its numbers:

| Metric | Transport suite | driver-bench | soak |
|---|---|---|---|
| Throughput (min–max) | avg/min/max publish + consume rate | avg/min/max rate + per-round detail | n/a (continuity harness) |
| Latency p50/p95/p99 | end-to-end publish→consume | per-op (`latency_ms.source` names the call) | n/a |
| Losses | `losses` | `losses` + `late_arrivals_after_drain` | `missing` |
| Duplicates | `duplicates` | worker: job-id dedup; dispatch: `null` (nothing consumed) | `duplicates` |
| Reconnects | `reconnects` (native pool `reconnects_total`; `null` for drivers without a counter) | `reconnects_total` (rabbit-rs; `null` elsewhere) | `reconnects_total` |
| Stall recoveries | `stall_recoveries` (0 by construction: stalls fail the run loudly) | `stall_recoveries` (0 by construction since Round I) | null streaks fail loudly |
| Safety mode | `safety` (safe/blind per scenario) | masked config echo (`safety`) | fills are confirmed |
| RabbitMQ + PHP configuration | `config` + `meta` (credentials masked) | masked config echo + `meta` | run parameters |

`null` always means "not measured for this driver", never "zero".

Archives recorded before Round J do not carry every field above (transport JSONs lack `safety`/`reconnects`/`config`/`meta`; driver-bench JSONs lack `duplicates`/`latency_ms`/`reconnects_total`). The tooling emits them from Round J on; the curated archives under `results/` are evidence and are never backfilled — each archive README notes its own gaps.

### Delivery semantics

Read every number with the delivery contract in mind:

- **At-least-once**: silent loss is unacceptable; duplicates are permitted, identifiable, and measured in every result set.
- The publisher replay buffer is **process-memory only**: unconfirmed publications survive connection recovery inside the same PHP process and are replayed with the same `message_id`, but they do **not** survive a PHP process crash.
- Cross-process durability requires an **external outbox**. No benchmark result constitutes a crash-durability guarantee.

### Reading and quoting results — workload-scoped framing only

Numbers are only comparable within one workload, one configuration, and one session. Quote them like this:

> On this workload (unit `Queue::push`, 1024 B Laravel envelope), with this configuration and these guarantees (safe mode: confirms + mandatory), rabbit-rs reaches X ops/s against Y ops/s for `<driver>` (same session, interleaved runs).

- State the driver semantics/configuration differences next to the numbers (confirms on/off, pop mechanism, prefetch, queue type) — the fairness tables in `driver-bench/README.md` list them.
- Compare only same-session interleaved runs; cross-session absolute deltas are session factors, not code signals.
- No absolute "N× faster" claims: quote the two throughput numbers against each other instead.

### Directory structure

```
benchmarks/
├── README.md
├── composer.json
├── docker-compose.yml       # Standalone RabbitMQ (for CI, uses rabbit_rs/rabbit_rs_lab)
├── run-benchmarks.sh         # Shell wrapper
├── driver-bench/              # Driver-level (Laravel queue API) benchmark app: bench.php + soak.php
├── baselines/
│   ├── check-budgets.php      # RC budget checker (anti-regression gate)
│   ├── reference-machine.json # Measured baseline + thresholds (in-repo)
│   └── smoke-budget.json      # Manual smoke thresholds
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
│   └── (scenarios are declared in run-benchmarks.php)
```

### Rust microbenchmarks

Criterion benchmarks for subsystem-level performance (if available):

```bash
cargo bench -p rabbit-rs-core
```
