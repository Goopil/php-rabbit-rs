# Recipe: Topology patterns

Which RabbitMQ pattern fits which Laravel workload, how each one maps to
Rabbit RS configuration, and how to gate the resulting topology in CI. The
mechanics (modes, declarations, recovery order) live in
[Topology](../topology.md).

## Pattern selection

| Pattern | Use when | Rabbit RS wiring |
|---|---|---|
| Work queue (competing consumers) | Background jobs of one kind | The default: one connection, one `queue` key, `--workers=N` |
| Weighted multi-queue | Job classes with different latency needs | `subscriptions` with `weight` / `priority_class` / per-subscription `prefetch` |
| Pub/sub (fan-out) | One event, several independent consumer groups | A topic exchange plus one subscription queue per group, each bound with its own routing key |
| Delayed jobs | `Job::dispatch()->delay(...)` | `delay.mode: auto` — plugin exchange when available, TTL buckets otherwise |
| Request/reply (RPC) | Service-to-service call/answer | Not yet built — milestone M3, see the [ROADMAP](../plans/ROADMAP.md) |

One rule cuts across all patterns: **jobs must be idempotent.** The
at-least-once contract redelivers anything not acknowledged, so an extra copy
is normal, counted, and possible after any reconnect
([Reliability](../reliability.md#duplicates)).

## Work queue — the default

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
tuning (see [Broker tuning](broker-tuning.md#prefetch-driver-side)) controls
buffering per worker, not parallelism.

## Pub/sub — several consumer groups, one event

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

## Dead-lettering poison safely

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

## Gate the topology in CI

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
transient consumer; in `verify`/`external` mode it is refused without
`--force`, because those modes promise externally managed topology.

## Which `topology_mode` per environment

| Environment | Mode | Why |
|---|---|---|
| Local / staging, driver owns topology | `declare` | DDL is idempotent and matches the config |
| Production with IaC (Terraform, management CLI) | `verify` | Catches drift without creating anything |
| Frozen platform, topology fully provisioned elsewhere | `external` | Zero declaration traffic |

Whatever the mode, `rabbit-rs:topology` verifies the same promises against the
live broker — see [Topology Mode](../topology.md#topology-modes).
