# Recipe: Broker tuning

The knobs that matter for a Laravel job pipeline live on two sides: the broker
(`rabbitmq.conf`, policies) and the driver (connection config). Listed in the
order they usually bite. Queue-type mechanics and declarations live in
[Topology](../topology.md).

## Queue type: quorum by default, classic when replication is not needed

`quorum` (the Rabbit RS default) is replicated with Raft consensus and
enforces `delivery_limit`; `classic` is a single-node durable queue — cheaper
per message, but a node loss takes its queues with it.

Rule of thumb: quorum for anything a job pipeline depends on (the default
exists so you have to *opt out*, not in); classic only for ephemeral,
high-churn queues where replication cost dominates and loss is acceptable by
design.

## Broker watermarks: what happens when the node is stressed

- `vm_memory_high_watermark` (default 60 % of RAM): when the node crosses it,
  the broker **blocks publishers**. With confirms on, this surfaces in Rabbit
  RS as rising confirm latency and `BackpressureDetected` events (the driver's
  bounded publish buffer fills) — never as silent loss.
- `disk_free_limit`: same publisher-blocking behavior when free disk falls
  below it.

Action: alert on `BackpressureDetected` and on the node's memory/disk metrics.
Do not "fix" backpressure by weakening confirms — it is the contract working.

## `max-length`: prefer rejection over silent trimming

If a queue is capped (via policies), set `overflow: reject-publish` in
reliable setups: the publisher gets a nack, which the at-least-once contract
surfaces as a visible error. The default `drop-head` silently deletes the
oldest messages — a silent-contract violation. A capped queue plus a DLX
(`x-overflow: reject-publish-dlx` on modern RabbitMQ) routes the rejected
messages where you can see them.

## Heartbeat and confirm timeout

- The connection-level `heartbeat` (seconds, driver side) detects half-open
  TCP — a crashed peer that never sent FIN. Keep the default; lower values
  detect dead peers faster at the cost of more broker chatter.
- `confirm_timeout` bounds how long a publish waits for its confirm. On a
  live connection a confirm timeout is **terminal** (the outcome is unknown,
  so nothing is replayed automatically); during a recovery, a parked
  publication is retried once with a fresh deadline. Sizing: the timeout must
  comfortably exceed worst-case broker stall, and the warning signs of
  approaching it are the watermarks above — see
  [Reliability — Publisher confirms](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/reliability.md#publisher-confirms).

## Prefetch (driver side) — the highest-leverage knob

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
[Subscriptions](https://github.com/Goopil/php-rabbit-rs/blob/main/docs/configuration.md).

## Verify the effect, not the intention

```bash
rabbitmqctl list_queues name messages_ready messages_unacknowledged
php artisan rabbit-rs:status --format=json   # same-process pool + management counters
php artisan rabbit-rs:doctor                 # wiring and safety summary
```

`rabbit-rs:status` cross-process counters (delivered / acked / redelivered
from the management API) are the ground truth for queue depth and duplicate
signal; `redelivered` also counts crash requeues, so treat it as an
approximate duplicate signal, not an error rate.
