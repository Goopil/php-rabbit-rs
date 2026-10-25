# Benchmark: rabbit-rs vs php-amqplib vs ext-amqp

Public, reproducible comparison of the three PHP RabbitMQ drivers on the same
broker, same message size, same QoS. It exists so the circulating "4–6×
consumer throughput" claim (curated-list announcements) can be checked
against a published methodology and archived raw data.

**Archive for the numbers below:
[`benchmarks/results/2026-10-25-issue-368/`](benchmarks/results/2026-10-25-issue-368/README.md)**
(raw per-driver JSONs in `raw/`, merged envelope
`comparison-results.json` — every number in this document appears in that
file). Historical archives: `benchmarks/results/round-*`, `flush-timer-latency/`.

## Summary (median of 5 interleaved cycles, 2026-10-25, msg/s)

| Scenario | rabbit-rs 0.3.12 | php-amqplib 3.7.4 | ext-amqp 2.2.0 |
|---|---|---|---|
| Publish, confirmed (50k × 1024 B, batches of 256) | 15,562 | 38,550 | **54,208** |
| Consume, push, manual ACK, prefetch 128 (50k × 1024 B) | 26,135 | 19,165 | **44,233** |
| Consume, Laravel-worker shape (10k × 1024 B, prefetch 64) | **16,518** | 1,520 | 1,677 |

Broker-kill under load (100k messages, full broker restart mid-drain): all
three drivers completed with **0 losses**; duplicates from redelivered
unacked work — rabbit-rs 46, amqplib 61, ext-amqp 20. rabbit-rs recovered
internally (1 harness restart), amqplib and ext-amqp needed the full
supervised restart on every attempt (7 of 8 attempts).

## What the numbers say — including the negative results

- **The raw push-consume comparison does not reproduce a 4–6× consumer
  advantage for rabbit-rs.** With every driver on its best push-consume path
  and identical ACK/QoS, rabbit-rs is 1.36× amqplib — and ext-amqp leads the
  cell. Quote the three numbers above, not a multiplier.
- **The large multiplier in circulation is a workload-shape effect.** On the
  Laravel-worker shape (the protocol the claim was made on), amqplib and
  ext-amqp drain via `basic_get` polling while rabbit-rs pushes: 16.5k vs
  ~1.5–1.7k (9.8–10.9× this session; 7.98× in the archived
  [round-l](benchmarks/results/round-l-post-302/README.md)). It measures
  polling vs push, not raw consume speed.
- **Confirmed publish is rabbit-rs' weakest cell** (15.6k vs 38.6k / 54.2k on
  this workload). At the 256 B payload of the round-l protocol the same cell
  read 32.2k vs 43.8k (0.74×) — payload-size sensitivity flagged, not
  investigated.
- At-least-once held everywhere: 0 losses on every measured throughput round
  and on all three broker-kill runs; duplicates only where redelivery is
  permitted (broker-kill).

Numbers are workload-scoped: quote them as "on this workload, with this
configuration and these guarantees" (framing rules:
[`benchmarks/README.md`](benchmarks/README.md#reading-and-quoting-results--workload-scoped-framing-only)).
No absolute "N× faster" claims.

## Scenario spec (version-pinned)

| Component | Version |
|---|---|
| Broker | RabbitMQ 4.2.9 (Erlang 27.3.4.16), image pinned to `rabbitmq@sha256:f093e74d…83095` in `lab/rabbitmq/rabbitmq/Dockerfile`, 3-node lab cluster |
| ext-rabbit_rs | 0.3.12, release cdylib, loaded per-run (a debug build masks throughput ~4×) |
| php-amqplib | v3.7.4 (`benchmarks/composer.lock`) |
| ext-amqp | 2.2.0 (pecl amqp) |
| PHP | 8.5.6 (rabbit-rs + amqplib), 8.4.21 (ext-amqp — pecl amqp ships for 8.4 locally) |
| Hardware | Apple M2 Max (Mac14,6), 12 cores, 32 GB, macOS 26.6.2, Docker 29.2.1 in colima (Virtualization.framework, virtiofs) |

All scenarios: 1024 B payload, durable classic queue, persistent delivery,
vhost `/`, 1 warmup cycle excluded. Per-driver configuration echoes (publish
mode, confirm batching, ACK granularity, prefetch, mandatory flag) are
recorded in the archive (`env.fairness` in `comparison-results.json`).

| Scenario | Count | Measured | Configuration |
|---|---|---|---|
| `publish-fanout` | 50,000 | publish rate | confirmed publication, batches of 256 |
| `sustained-consume` | 50,000 | consume rate | blind batched fill → push consume, manual ACK per message, prefetch 128 |
| `laravel-worker` | 10,000 | consume rate | historical round-l protocol, prefetch 64, basic_get polling on amqplib/ext-amqp |
| `broker-kill` | 100,000 | survival / losses / duplicates / recoveries | `docker restart` of the broker node armed 2 s into the drain; identical supervised restart policy for every driver |

## One-command reproduction

```bash
./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh
composer install --working-dir=benchmarks
./benchmarks/run-comparison.sh            # OUT_DIR=… to choose the archive dir, ROUNDS=5 by default
```

Requirements: Docker (the lab), PHP 8.5 CLI (`php`), a PHP 8.4 binary with
pecl amqp (`PHP84=/opt/homebrew/opt/php@8.4/bin/php` by default on macOS),
Rust toolchain (the script builds the release extension when missing —
one-time, slow). The script builds/loads the extension per-run, interleaves
the three drivers per cycle, runs the broker-kill scenario, and merges
everything into `<OUT_DIR>/comparison-results.json` with the integrity gates
(losses == 0; non-zero duplicates fail a throughput round).

## Metric contract

Latency percentiles, losses, duplicates, reconnects and per-driver config are
embedded in every run row (Round J contract,
[`benchmarks/README.md`](benchmarks/README.md#published-result-metric-contract-round-j-127)).
`null` means "not measured for this driver", never "zero". Delivery
semantics: at-least-once; the replay buffer is process-memory only and no
benchmark result implies crash durability.
