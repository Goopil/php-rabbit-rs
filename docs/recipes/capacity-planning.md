# Recipe: Capacity planning

Use the published evidence as a starting point, then measure **your** workload
— numbers are only comparable within one workload, one configuration, and one
session ([the framing rules](../../benchmarks/README.md#reading-and-quoting-results--workload-scoped-framing-only)).

## What the published evidence covers

**Comparative (lab workloads):** on the curated lab workloads, rabbit-rs
consumes 4–6× faster than php-amqplib in the same session, with 0 losses and
0 duplicates in every reliable-mode run. That is a workload-scoped
measurement, not a promise for your workload — quote both throughput numbers
with their configuration, never a bare multiplier.

**Stability (Round K soak** — 3-node lab, release build, archived under
`benchmarks/results/round-k-soak/`):

- Steady 30 min: 5.9 M messages pop+ack (≈3.3k/s on the lab hardware), 0 loss,
  1 duplicate, RSS plateau after warmup (envelope slope +0.6 MB/h).
- Kill 60 min: 2.9 M messages across 297 forced connection kills, **0
  missing**, 17,655 duplicates (0.6 %), publish-buffer tripwire never fired,
  RSS bounded.

Expectation setting from the kill run: **duplicates under connection churn
are contract behavior, counted, not anomalies** — 0.6 % under a kill every 12
seconds is what aggressive recovery churn looks like.

## Sizing your deployment

1. **Measure, don't extrapolate.** Run the harness on production-like
   hardware and realistic payload/job durations:

   ```bash
   ./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh
   ./benchmarks/run-benchmarks.sh --driver=rabbit-rs --scenario=laravel-worker
   # Stability + memory evidence (steady and kill modes):
   php benchmarks/driver-bench/bin/soak.php --minutes=30 --kill-every=0
   php benchmarks/driver-bench/bin/soak.php --minutes=60 --kill-every=10
   ```

2. **Job duration is the driver.** Steady-state throughput per worker is
   ≈ 1/mean-job-duration; scale `--workers` per connection and add consumer
   capacity per queue. Adaptive prefetch (Round E) keeps
   ~`target_buffer_seconds` of ready work buffered, so a burst does not starve
   a busy subscription — tune that target, not a fixed prefetch guess.

3. **Broker capacity.** Quorum queues cost more per message (replication +
   Raft) than classic — that is the price of surviving node loss, and the
   default here for good reason. Watch `messages_unacknowledged` alongside
   queue depth: slow consumers convert prefetch into resident node memory
   (see [Broker tuning](broker-tuning.md#broker-watermarks-what-happens-when-the-node-is-stressed)).

4. **Memory expectations.** The soak envelope is the model: RSS is bounded
   after warm-up; a genuine leak must exceed the warmup peak and keep
   climbing. Keep the soak in CI (nightly, `--leak-mb-per-hour`, default
   20 MB/h) so a regression fails a run instead of a pager.

5. **Read backpressure as a capacity signal.** `BackpressureDetected` events
   and rising `backpressure_total` in `rabbit-rs:status` mean the publisher
   side is saturated: add workers or shard connections before raising
   `confirm_timeout`.

## Red flags

- Duplicates rising on a steady run (no kills, no restarts) → investigate;
  do not normalize it.
- `publish_buffered > 0` at rest in `Pool::stats()` → publications parked
  across cycles (the re-buffer leak path the soak tripwire watches).
- RSS climbing past the warm-up peak → rerun the envelope estimator from the
  Round K evidence before blaming the workload.
