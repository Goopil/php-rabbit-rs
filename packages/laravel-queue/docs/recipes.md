# Recipes

Task-oriented guides on top of the [reference](reference.md): map a Laravel workload to RabbitMQ wiring, tune the broker, and size a deployment from measured evidence.

| Recipe | Use it when |
|---|---|
| [Topology patterns](#recipe-topology-patterns) | Mapping Laravel workloads to work-queue / pub-sub / delayed / dead-letter wiring, and gating that topology in CI |
| [Broker tuning](#recipe-broker-tuning) | Choosing queue types, broker watermarks, and prefetch for predictable behavior under load |
| [Capacity planning](#recipe-capacity-planning) | Sizing workers, queues, and brokers from the measured evidence instead of guesses |

## Recipe: Topology patterns

Which RabbitMQ pattern fits which Laravel workload, how each one maps to
Rabbit RS configuration, and how to gate the resulting topology in CI. The
mechanics (modes, declarations, recovery order) live in
[Topology](reference.md#topology).

### Pattern selection

| Pattern | Use when | Rabbit RS wiring |
|---|---|---|
| Work queue (competing consumers) | Background jobs of one kind | The default: one connection, one `queue` key, `--workers=N` |
| Weighted multi-queue | Job classes with different latency needs | `subscriptions` with `weight` / per-subscription `prefetch` |
| Pub/sub (fan-out) | One event, several independent consumer groups | A topic exchange plus one subscription queue per group, each bound with its own routing key |
| Delayed jobs | `Job::dispatch()->delay(...)` | `delay.mode: auto` — a documented alias for the delayed-message plugin; no TTL fallback (an unroutable delay fails terminally). `ttl` remains available as an explicit mode |
| Request/reply (RPC) | Service-to-service call/answer | Not yet built — milestone M3, see the [ROADMAP](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/plans/ROADMAP.md) |

One rule cuts across all patterns: **jobs must be idempotent.** The
at-least-once contract redelivers anything not acknowledged, so an extra copy
is normal, counted, and possible after any reconnect
([Reliability](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#duplicates)).

### Work queue — the default

A single queue with competing consumers is what `queue:work` semantics mean,
and the default config is exactly that:

```php
'rabbit-rs' => [
    'driver' => 'rabbit-rs',
    'queue'  => 'orders',
    'hosts'  => [['host' => 'rabbit-1', 'port' => 5672]],
],
```

Scale by adding supervisor children, not prefetch: `php artisan rabbit-rs:work
--connection=rabbit-rs --workers=4` spawns four children that each consume the
connection's whole queue set through the weighted-fair scheduler. Prefetch
tuning (see [Broker tuning](#prefetch-driver-side--the-highest-leverage-knob)) controls
buffering per worker, not parallelism.

### Pub/sub — several consumer groups, one event

Publish to a topic exchange and let each consumer group own a queue bound with
the routing keys it cares about. In Rabbit RS, every subscription is a queue
on the connection's exchange:

```php
'subscriptions' => [
    'audit'    => ['queue' => 'audit.trail',    'weight' => 1],
    'billing'  => ['queue' => 'billing.events', 'weight' => 4],
],
```

Each subscription gets its own dedicated channel and prefetch. Groups that
should never slow each other down belong on separate *connections* — a
subscription cannot cross brokers.

### Dead-lettering poison safely

Two rules from the reference docs, repeated here because they bite in
production:

- `delivery_limit` (quorum queues) caps redeliveries — **`dead_letter` MUST be
  configured when it is set**, or poison messages are silently dropped after
  the limit.
- Without any dead-letter config, Rabbit RS creates no DLQ at all.

```php
'queue_type'    => 'quorum',
'delivery_limit' => 20,
'dead_letter'   => ['exchange' => 'dead-letters', 'queue' => 'failed-jobs'],
```

### Gate the topology in CI

Make the broker contract a deploy check, not a hope:

```bash
# Read-only: verify every subscription queue, the DLX, its binding,
# and each queue's x-queue-type (management_url required for the latter)
php artisan rabbit-rs:topology --connection=rabbit-rs

# Full health report: extension version vs composer constraint, broker
# reachability, publisher wiring, safety summary — CI-friendly exit codes
php artisan rabbit-rs:doctor
```

Both exit non-zero on failure and name the exact config path of anything
missing — wire them into your deploy pipeline before the workers roll. In
`declare` mode, `rabbit-rs:topology --fix` declares missing items through a
transient consumer, then re-verifies every declared object before reporting
success: each subscription queue through the passive probe, and (with
`management_url`) the route exchange, its bindings and each queue's
`x-queue-type` through the management API. `topology declared` is only
printed once those checks confirm the objects on the broker — any object the
declare failed to land prints its own failure line and fails the command
(non-zero exit), so a repair run can never report a fix it did not make. A
consumer-readiness timeout still only warns: the declaration lands before
consumer channels start, so `--fix` stays usable before any worker exists.
In `verify`/`external` mode `--fix` is refused without `--force`, because
those modes promise externally managed topology.

### Which `topology_mode` per environment

| Environment | Mode | Why |
|---|---|---|
| Local / staging, driver owns topology | `declare` | DDL is idempotent and matches the config |
| Production with IaC (Terraform, management CLI) | `verify` | Catches drift without creating anything |
| Frozen platform, topology fully provisioned elsewhere | `external` | Zero declaration traffic |

Whatever the mode, `rabbit-rs:topology` verifies the same promises against the
live broker — see [Topology Mode](reference.md#topology-modes).

## Recipe: Broker tuning

The knobs that matter for a Laravel job pipeline live on two sides: the broker
(`rabbitmq.conf`, policies) and the driver (connection config). Listed in the
order they usually bite. Queue-type mechanics and declarations live in
[Topology](reference.md#topology).

### Queue type: quorum by default, classic when replication is not needed

`quorum` (the Rabbit RS default) is replicated with Raft consensus and
enforces `delivery_limit`; `classic` is a single-node durable queue — cheaper
per message, but a node loss takes its queues with it.

Rule of thumb: quorum for anything a job pipeline depends on (the default
exists so you have to *opt out*, not in); classic only for ephemeral,
high-churn queues where replication cost dominates and loss is acceptable by
design.

### Broker watermarks: what happens when the node is stressed

- `vm_memory_high_watermark` (default 60 % of RAM): when the node crosses it,
  the broker **blocks publishers**. With confirms on, this surfaces in Rabbit
  RS as rising confirm latency and `BackpressureDetected` events (the driver's
  bounded publish buffer fills) — never as silent loss.
- `disk_free_limit`: same publisher-blocking behavior when free disk falls
  below it.

Action: alert on `BackpressureDetected` and on the node's memory/disk metrics.
Do not "fix" backpressure by weakening confirms — it is the contract working.

### `max-length`: prefer rejection over silent trimming

If a queue is capped (via policies), set `overflow: reject-publish` in
reliable setups: the publisher gets a nack, which the at-least-once contract
surfaces as a visible error. The default `drop-head` silently deletes the
oldest messages — a silent-contract violation. A capped queue plus a DLX
(`x-overflow: reject-publish-dlx` on modern RabbitMQ) routes the rejected
messages where you can see them.

### Heartbeat and confirm timeout

- The connection-level `heartbeat` (seconds, driver side) detects half-open
  TCP — a crashed peer that never sent FIN. Keep the default; lower values
  detect dead peers faster at the cost of more broker chatter.
- `confirm_timeout` bounds how long a publish waits for its confirm. On a
  live connection a confirm timeout is **terminal** (the outcome is unknown,
  so nothing is replayed automatically); during a recovery, a parked
  publication is retried once with a fresh deadline. Sizing: the timeout must
  comfortably exceed worst-case broker stall, and the warning signs of
  approaching it are the watermarks above — see
  [Reliability — Publisher confirms](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reference.md#publisher-confirms).

### Prefetch (driver side) — the highest-leverage knob

Prefetch bounds how many unacked messages one consumer holds — it throttles
the worker *and* bounds the broker's acker memory, because unacked messages
sit on the node until acknowledged:

- **Fixed** — `prefetch => 32`: predictable, right for uniform job durations.
- **Adaptive** — keeps about `target_buffer_seconds` of ready work buffered,
  learning job duration (EWMA) and adjusting between `min` and `max` with
  hysteresis. Right when job durations vary, so slow jobs do not stall the
  pipeline while fast jobs starve it.

```php
'subscriptions' => [
    'critical' => [
        'queue' => 'orders.critical',
        'prefetch' => [
            'mode' => 'adaptive',
            'initial' => 64, 'min' => 1, 'max' => 256,
            'target_buffer_seconds' => 5,
        ],
    ],
],
```

A runaway prefetch under slow consumers is a broker memory hazard: the node
holds every unacked delivery. Adaptive prefetch is the default-shaped answer
when in doubt. Full option reference:
[Subscriptions](reference.md#subscriptions-escape-hatch).

### Verify the effect, not the intention

```bash
rabbitmqctl list_queues name messages_ready messages_unacknowledged
php artisan rabbit-rs:status --format=json   # same-process pool + management counters
php artisan rabbit-rs:doctor                 # wiring and safety summary
```

`rabbit-rs:status` cross-process counters (delivered / acked / redelivered
from the management API) are the ground truth for queue depth and duplicate
signal; `redelivered` also counts crash requeues, so treat it as an
approximate duplicate signal, not an error rate.

## Recipe: Capacity planning

Use the published evidence as a starting point, then measure **your** workload
— numbers are only comparable within one workload, one configuration, and one
session ([the framing rules](https://github.com/Goopil/php-rabbit-rs/blob/main/benchmarks/README.md#reading-and-quoting-results--workload-scoped-framing-only)).

### What the published evidence covers

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

### Sizing your deployment

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
   (see [Broker tuning](#broker-watermarks-what-happens-when-the-node-is-stressed)).

4. **Memory expectations.** The soak envelope is the model: RSS is bounded
   after warm-up; a genuine leak must exceed the warmup peak and keep
   climbing. Keep the soak in CI (nightly, `--leak-mb-per-hour`, default
   20 MB/h) so a regression fails a run instead of a pager.

5. **Read backpressure as a capacity signal.** `BackpressureDetected` events
   and rising `backpressure_total` in `rabbit-rs:status` mean the publisher
   side is saturated: add workers or shard connections before raising
   `confirm_timeout`.

### Red flags

- Duplicates rising on a steady run (no kills, no restarts) → investigate;
  do not normalize it.
- `publish_buffered > 0` at rest in `Pool::stats()` → publications parked
  across cycles (the re-buffer leak path the soak tripwire watches).
- RSS climbing past the warm-up peak → rerun the envelope estimator from the
  Round K evidence before blaming the workload.
