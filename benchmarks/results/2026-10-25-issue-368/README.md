# Issue #368 — public driver comparison: rabbit-rs vs php-amqplib vs ext-amqp

Date: 2026-10-25. Issue: #368 (public, reproducible comparison; the "4–6×
consumer throughput" claim must have a public methodology).

Extension: **release** cdylib (`target/release/librabbit_rs_php.dylib`,
rabbit_rs-php 0.3.12), loaded per-run with `-d extension=...`, never installed
system-wide. Harness: `benchmarks/src/run-comparison.php` +
`benchmarks/run-comparison.sh` (one command). Raw per-invocation JSONs:
`raw/partial-<driver>.json`; merged envelope: `comparison-results.json` —
every number quoted in `BENCHMARK.md` is present in that file.

## Environment (pinned)

- Broker: local RabbitMQ lab, `./scripts/lab-up.sh with-plugin`; image pinned
  in `lab/rabbitmq/rabbitmq/Dockerfile` to
  `rabbitmq@sha256:f093e74d14814d28e3d52e7dee5873ab8e8c2e671e9e11019654bd3443183095`
  + delayed-message-exchange 4.2.0 plugin — **RabbitMQ 4.2.9**, Erlang
  27.3.4.16, 3-node cluster; clients connect to node 1 (127.0.0.1:5672),
  vhost `/`.
- PHP: 8.5.6 (cli, Homebrew) for rabbit-rs and amqplib; 8.4.21
  (`/opt/homebrew/opt/php@8.4`, ships pecl amqp) for ext-amqp.
- Drivers: ext-rabbit_rs 0.3.12 (release cdylib); php-amqplib v3.7.4
  (composer.lock); ext-amqp 2.2.0 (phpversion('amqp') in
  `raw/partial-amqp-ext.json`).
- Hardware: Apple M2 Max (Mac14,6), 12 cores, 32 GB, macOS 26.6.2, Docker
  29.2.1 in colima (Virtualization.framework, virtiofs).
- Workload: 1024 B payload, durable classic queue, persistent delivery.

## Protocol

5 interleaved cycles × 3 scenarios × 3 drivers; one cycle alternates
rabbit-rs → amqplib → amqp-ext per scenario (round-level interleaving is
impossible across PHP binaries; the cycle keeps the drift-sharing
alternation of the release protocol). 1 warmup round (cycle 1, archived with
`warmup: true`, excluded from medians). Integrity gates (losses == 0 &&
duplicates == 0) held on every measured throughput round of every driver.

| Scenario | Measured | Configuration |
|---|---|---|
| `publish-fanout` (50,000 msgs) | publish rate | confirmed publication, batches of 256, persistent; drain unmeasured |
| `sustained-consume` (50,000 msgs, the headline claim) | consume rate | blind batched fill (unmeasured) → push consume, manual ACK per message, prefetch 128 on every driver |
| `laravel-worker` (10,000 msgs, historical lineage) | consume rate | round-l protocol: prefetch 64; rabbit-rs dedicated consumer pop+ACK, amqplib/ext-amqp basic_get polling |
| `broker-kill` (100,000 msgs, single run per driver) | survival, losses, duplicates, recoveries | full `docker restart` of node 1 armed 2 s into the drain; identical harness-level driver restart between attempts for every driver |

Per-driver configuration echoes are embedded in `comparison-results.json`
under `env.fairness`.

## Results (median of 5 cycles, msg/s)

| Scenario | rabbit-rs | amqplib | amqp-ext |
|---|---|---|---|
| publish-fanout publish | 15,562 (14.4k–18.8k) | 38,550 (26.2k–39.8k) | **54,208** (51.7k–67.4k) |
| sustained-consume consume | 26,135 (18.5k–36.1k) | 19,165 (18.4k–19.5k) | **44,233** (30.0k–53.1k) |
| laravel-worker consume | **16,518** (12.4k–23.8k) | 1,520 (1.5k–2.2k) | 1,677 (1.7k–2.5k) |

Ratios (same session, interleaved): sustained-consume rabbit-rs/amqplib
1.36×, ext-amqp/rabbit-rs 1.69×; laravel-worker rabbit-rs/amqplib 10.9×,
rabbit-rs/ext-amqp 9.8×.

## Findings (negative results included, as-is)

1. **The raw push-consume comparison does NOT reproduce a 4–6× consumer
   advantage for rabbit-rs.** With every driver on its best push-consume path
   (`basic_consume` / `AMQPQueue::consume` / dedicated consumer), manual ACK
   per message, prefetch 128, rabbit-rs reads 26.1k msg/s vs amqplib 19.2k
   (1.36×) — and **ext-amqp leads the cell at 44.2k**. Anyone quoting a
   large consumer multiplier for rabbit-rs must name the workload shape (see
   finding 2).
2. **The historical 4–6×+ claim comes from the Laravel-worker shape**, where
   amqplib/ext-amqp drain via `basic_get` polling while rabbit-rs uses its
   push consumer: 16.5k vs 1.5k/1.7k (9.8–10.9× this session; round-l
   measured 7.98× the same way). The multiplier measures the pop mechanism
   (polling vs push), not raw consume speed.
3. **Confirmed publish is rabbit-rs' weakest cell**: 15.6k msg/s vs 38.6k
   (amqplib) and 54.2k (ext-amqp) on 256-batch confirms at 1024 B. Round-l
   measured the same cell at 256 B payload where rabbit-rs published 32.2k vs
   amqplib 43.8k (0.74×) — payload-size sensitivity worth a dedicated look,
   not investigated here.
4. **Broker-kill under load: all three drivers survived with zero losses**
   (100k msgs, full container restart mid-drain), because the harness applies
   the same supervised-restart policy to every driver. Differences: rabbit-rs
   needed 1 harness recovery / 2 attempts (its pool recovers internally,
   `reconnects_total` 0 after the successful re-establish) vs 7 recoveries /
   8 attempts for amqplib and ext-amqp, whose connections died with the
   broker (`CONNECTION_FORCED` on rabbit-rs, dead-socket channel errors on
   the others — recorded as-is in `last_error`). Duplicates (redelivered
   unacked work, permitted under at-least-once): rabbit-rs 46, amqplib 61,
   ext-amqp 20.

## Session notes (variance, disclosed)

- Cycle 3 shows a session-wide dip hitting every driver in the same cycle
  (e.g. sustained-consume rabbit-rs 18.5k and ext-amqp 30.0k vs ~36k/~53k in
  cycle 1) — host/broker drift, which the interleaved cycles and medians are
  there to absorb. Cycle-level medians, not best-case runs, are quoted.
- The lab's `without-plugin` compose profile is currently broken (the plain
  `prometheus` service lost its `profiles:` gate and depends on
  profile-gated `rabbitmq-1`); this session used `with-plugin` (as the
  earlier archives did). toxiproxy failed to bind on this host (ports
  24500–24509 held by an unrelated ssh tunnel) — not used by these scenarios.
- One runner bug found during the session: the merge step rejected the bare
  `--merge` flag (option parser required `--key=value`); fixed in the same
  session, no measurement was affected (merging is post-processing).

## Files

- `comparison-results.json` — merged envelope: env (broker/hardware/versions/
  fairness echoes) + every run (warmups included) + meta.
- `raw/partial-<driver>.json` — the per-driver invocation outputs the merge
  consumed.
