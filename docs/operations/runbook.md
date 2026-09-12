# Operations Runbook

Incident playbooks for the Rabbit RS observability model. v1 ships **no
exporter**: per-process native metrics are collected externally (a sidecar
polling `php artisan rabbit-rs:status --format=json`, or listeners on the
Laravel events — see the *Sidecar exporter* section of
`packages/laravel-queue/docs/reference.md`), and cluster-level signals come
from the RabbitMQ management API or the RabbitMQ Prometheus plugin.

> **Prerequisite.** Production RabbitMQ must have the management metrics
> collector enabled (the `rabbitmq_prometheus` plugin, or the management API
> with `management_agent.disable_stats = false`) or the per-queue gauges below
> are missing entirely. Fresh queues can also lag or omit gauges until the
> stats collector emits its first sample — the same caveat as the FPM harness
> observer in `scripts/test-fpm.sh`. A gauge that is absent is *not* a zero.

Per-process metrics (`Pool::stats()` keys, surfaced by
`rabbit-rs:status --format=json`) are **same-process by design**: aggregate
with `sum()` across worker instances in your monitoring system, and remember
that a freshly started CLI process (including `rabbit-rs:status` itself)
reads zero until it does work.

Signals referenced below, with their source of truth:

| Signal | Surface |
|--------|---------|
| `reconnects_total` | `Pool::stats()` / `crates/rabbit-rs-core/src/metrics.rs` |
| `backpressure_total` | `Pool::stats()` / `metrics.rs` |
| `dropped_publications_total` | `Pool::stats()` (`crates/rabbit-rs-php/src/classes/pool.rs`) |
| `dropped_error_records_total` | `Pool::stats()` |
| `publish_buffered` / `publish_buffered_bytes` | `Pool::stats()` |
| `duplicates_total` | `Pool::stats()` / `metrics.rs` |
| `publication_retries_total` | `Pool::stats()` / `metrics.rs` |
| `confirmation_latency_p50/p95/p99` | `Pool::stats()` (ms) |
| `settlement_latency_p50/p95/p99` | `Pool::stats()` (ms) |
| `ConnectionStateChanged` | Laravel event `Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged` |
| `BackpressureDetected` | Laravel event `Goopil\RabbitRs\Laravel\Events\BackpressureDetected` |
| `rabbit-rs:doctor` | artisan CLI: per-connection extension + broker probe |
| `messages`, `messages_redelivered` | management API `GET /api/queues/{vhost}/{queue}` |

Alert rules over these signals live in [alerts.md](alerts.md); a Grafana
dashboard definition in [dashboard.json](dashboard.json).

---

## 1. Broker down / reconnect storm

**Symptom.** Worker logs show repeated recovery cycles; publications
stall; jobs sit in the queue; consumers stop receiving.

**Signals.**

- `reconnects_total` climbing across worker instances (`Pool::stats()` or the
  sidecar series).
- `ConnectionStateChanged` events (Laravel) — wire them to your monitoring
  and log the state transitions; they fire during publish/consume operations,
  no polling required.
- `php artisan rabbit-rs:doctor [--connection=<name>]` — active probe: checks
  the extension and the broker reachability per connection. Run it on any
  node that can reach the broker.
- Broker side: management API `GET /api/health/checks/individual/node` or
  `rabbitmqctl cluster_status`.

**Action.**

1. Check broker cluster health first (the outage is usually upstream).
2. Confirm the PHP workers are *buffering, not failing*: `publish_buffered`
   should hold the unconfirmed publications and drain back to 0 after
   recovery; `reconnects_total` stops climbing once the connection is stable.
3. Unconfirmed publications survive only in bounded process memory (replayed
   with the original deadline); if the buffer capacity was exhausted during
   the outage, producers received `Backpressure` — see incident 2.
4. After the storm, review `publication_retries_total`: it counts publications
   whose deadline expired while parked during a recovery suspension and were
   re-armed once before failing terminally.

## 2. Backpressure growth

**Symptom.** Publish calls start throwing/rejecting with `Backpressure` as
the bounded publisher budget (1024 publications / 64 MiB by default)
exhausts; throughput collapses at the producer.

**Signals.**

- `backpressure_total` increasing (`Pool::stats()`, `metrics.rs`).
- `BackpressureDetected` event (Laravel) carrying `broker`, `inFlight`,
  `capacity` — dispatches the moment capacity is hit, no polling required.
- `publish_buffered` / `publish_buffered_bytes` near the configured capacity.

**Action.**

1. Slow the producers (batch, rate-limit) or scale consumers to drain the
   confirm pipeline.
2. Check `confirmation_latency_p99` and the broker load: sustained
   backpressure usually means broker saturation, not a driver bug.
3. Raise the buffer capacity only for a genuinely higher steady-state load —
   it is a bounded safety valve, not a queue.

## 3. Dropped publications ≠ 0 — PAGE IMMEDIATELY

**Symptom.** None at the application layer: a publication accepted by the
confirmed path silently vanished. This is the one incident class that is a
data-loss bug by contract — at-least-once delivery forbids silent loss after
acceptance.

**Signals.**

- `dropped_publications_total` > 0 in `Pool::stats()` (per process).
- `dropped_error_records_total` growing alongside it (the drainable error
  records that describe what was dropped; read them with `drainErrors()`).

**Action.**

1. **Page immediately.** Preserve the process (do not restart workers) and
   capture `stats()` plus the drained error records for the affected pools.
2. Check whether a graceful teardown path was skipped (SIGKILL, `exit()`
   without `flush()`/pool close): the publish buffer is in-memory only and is
   not crash-durable — publications parked in it die with the process. That
   is a *documented* loss path for ungraceful termination; a non-zero counter
   outside it is a bug.
3. Compare `dropped_publications_total` against the process restart/kill
   timeline to classify: expected teardown drop vs confirmed-path loss.
4. File an incident referencing the captured error records; recover the lost
   payloads from the outbox if one exists, else from the producing side.

## 4. Stuck publish buffer

**Symptom.** `publish_buffered` plateaus above zero while the process is
idle or after `flush()` — or `publish_buffered_bytes` grows while
`publish_buffered` stays flat (payloads accumulating without progress).

**Signals.**

- `publish_buffered` plateau vs `publish_buffered_bytes` growth in
  `Pool::stats()`.
- `confirmations_total` stalled: confirms stopped arriving but publishes
  continue.

**Action.**

1. Check broker confirm flow and connectivity (`rabbit-rs:doctor`).
2. Call `flush()` on the pool (or let the request/worker teardown flush) and
   re-read `stats()`. A healthy buffer quiesces to 0 — the soak tripwire
   asserts exactly that, and a re-buffer leak is a bug.
3. If the buffer only drains on teardown, inspect the flush-interval config:
   a very large age-flush interval parks publications longer by design (the
   FPM/Octane scenarios rely on the same timer).

## 5. Duplicates spike

**Symptom.** Jobs executing twice; downstream idempotency violations.

**Signals.**

- `duplicates_total` per process: deliveries the broker flagged as
  redeliveries and *that process* settled. Watch for a jump correlated with
  `reconnects_total` (each recovery can replay or re-deliver).
- Cross-process: management API `messages_redelivered` — an **approximate**
  duplicate signal only. At-least-once also redelivers after consumer crashes
  and stale-ACK rejections, so redeliveries are not necessarily duplicates;
  it also counts crash requeues. Never page on `messages_redelivered` alone —
  correlate with reconnects and consumer crash logs.

**Action.**

1. Verify consumers are idempotent (`message_id` / business keys; the
   delivery `attempts()` count exposes `x-delivery-count`).
2. Correlate the spike window with reconnects (incident 1) or worker crashes;
   a spike without either is worth a bug report with per-process counters.
3. `delivery_limit` + `dead_letter` bounds the damage for genuinely poisoned
   duplicates (see incident 6).

## 6. Poison messages (terminal settle → DLQ)

**Symptom.** A message repeatedly fails processing until it hits its
attempts cap and is dead-lettered instead of retried forever.

**Signals.**

- Broker side: depth growth on the dead-letter queue (management API
  `GET /api/queues/{vhost}/{dlq}` → `messages`) and `messages` dropping on
  the source queue without matching success metrics.
- Consumer side: `rejects_total` in `Pool::stats()` climbing with no
  matching `acks_total`.

**Action.**

1. Configure the trap ahead of time: set `delivery_limit` (quorum queues)
   together with `dead_letter` — the driver requires the DLQ when a delivery
   limit is set, so terminal settle has a destination.
2. Inspect the DLQ payloads to classify the poison (malformed payload, bug,
   dependency outage) and requeue or discard deliberately.
3. If messages dead-letter without ever being attempted, check generation
   staleness: ACKs from a connection generation older than the current one
   are rejected so the broker redelivers — a consumer that holds deliveries
   across a recovery without re-settling will feed this path.

## 7. Consumer settlement latency

**Symptom.** Queue drain rate drops; jobs complete late; downstream SLAs
suffer.

**Signals.**

- `settlement_latency_p95` / `settlement_latency_p99` (ms) in
  `Pool::stats()` — delivery-to-settlement end-to-end latency percentiles.
- Correlate with `delivery prefetch`/QoS settings, worker count, and broker
  load (`rabbitmq_queue_messages` trend on the management API/plugin).

**Action.**

1. Raise consumer parallelism (workers, prefetch/QoS) before raising
   timeouts.
2. A latency spike together with `reconnects_total` growth points at
   recovery suspensions rather than slow handlers.
3. Sustained growth with a healthy broker usually means the consumers are the
   bottleneck — scale them (this is also the response to incident 2).
