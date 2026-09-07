# Getting started — native extension

The `rabbit_rs` extension is a standalone PHP extension written in Rust. It has no framework dependency: this guide uses plain PHP only. If you use Laravel, the [Laravel queue driver](https://github.com/Goopil/php-rabbit-rs/tree/main/packages/laravel-queue) wraps everything below behind the standard `Queue` API — see the [Laravel getting started](https://github.com/Goopil/php-rabbit-rs/blob/main/packages/laravel-queue/docs/getting-started.md).

This page progresses from a minimal working setup to production operation. The full API signatures live in the generated stub: [`crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`](https://github.com/Goopil/php-rabbit-rs/blob/main/crates/rabbit-rs-php/stubs/rabbit_rs.stub.php).

## Install

```bash
pie install goopil/rabbit-rs-native
php --ri rabbit_rs   # verify it loads
```

Details (macOS, Docker, manual binaries, rollback): [Installation](reference.md#installation).

## 1. Hello world

Everything starts with a `Pool` built from a plain configuration array: one or more **brokers**, and one or more **worker profiles** listing the queues to consume.

```php
use Goopil\RabbitRs\Pool;

$config = [
    'brokers' => [[
        'name' => 'default',
        'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
        'vhost' => '/',
        'credentials' => ['username' => 'guest', 'password' => 'guest'],
        'tls' => ['enabled' => false],
        'heartbeat' => 30,
    ]],
    'workers' => [[
        'name' => 'default',
        'subscriptions' => [[
            'name' => 'default',
            'broker' => 'default',
            'queue' => 'hello',
            'weight' => 1,
            'priority_class' => 0,
            'prefetch' => 16,
        ]],
        'scheduler' => ['strategy' => 'weighted_fair'],
    ]],
    'topology_mode' => 'declare',
];

$pool = new Pool($config);
```

Publish one message through the default exchange (`''` routes directly to the queue named by the routing key). Publishing is asynchronous internally: `flush()` blocks until every buffered publication is confirmed.

```php
$pool->publish([
    'broker' => 'default',
    'exchange' => '',
    'routing_key' => 'hello',
    'payload' => json_encode(['hello' => 'world']),
    'message_id' => uniqid('', true),
    'timeout_ms' => 5000,
]);
$pool->flush();
```

Consume in a loop. `next($timeoutMs)` returns the next delivery or `null` on timeout; `ack()` settles it.

```php
$consumer = $pool->consumer('default'); // the worker profile name

while (true) {
    $delivery = $consumer->next(1000);
    if ($delivery === null) {
        continue; // nothing ready within 1 s
    }

    echo $delivery->payload(), PHP_EOL;
    $delivery->ack();
}
```

Because `topology_mode` is `declare`, the queue is created on first use. Use `verify` to check an externally provisioned topology, or `external` to never touch it — see [Topology management](https://github.com/Goopil/php-rabbit-rs/blob/main/packages/laravel-queue/docs/reference.md#topology).

## 2. Reliability

Delivery is **at-least-once**: once a message is accepted into the confirmed delivery path, silent loss is unacceptable, and duplicates are permitted and measurable. Your processing must be idempotent — the `message_id` you publish is the stable deduplication key. The full contract: [Reliability](reference.md#reliability).

**Safety modes** — one key controls the publisher guarantees (`publisher.safety`):

| Mode | Behaviour |
| ---- | --------- |
| `safe` (default) | Publisher confirms + mandatory routing; every publish resolves to confirmed, returned, or timed out. Unroutable messages come back instead of being dropped. |
| `unsafe` | A synchronous socket write with no outcome tracking. Unroutable messages are silently dropped. |
| `blind` | Explicit fire-and-forget through a bounded background pump; a transport failure after hand-off is a silent loss. |

Batch publishing crosses the PHP/Rust boundary once for up to 256 messages:

```php
$pool->publishBatch(array_map(fn (int $i) => [
    'broker' => 'default',
    'exchange' => '',
    'routing_key' => 'hello',
    'payload' => "message {$i}",
    'message_id' => uniqid('', true),
    'timeout_ms' => 5000,
], range(1, 100)));
$pool->flush(); // barrier: everything above is confirmed (or thrown) when this returns
```

**When things fail.** Two exceptions matter:

- `Goopil\RabbitRs\BackpressureException` — the bounded publish buffer is full (outage with sustained traffic). Retry with the same message later; already-buffered messages are never dropped.
- `Goopil\RabbitRs\ConnectionException` — a connection-level failure. The pool recovers automatically (deterministic order: connection, channels, topology, publisher replay, consumers); unconfirmed publications are replayed with their original `message_id` inside the same PHP process.

Publish outcomes that could not be delivered synchronously (a confirm timeout during a recovery window, for instance) surface at the next operation or explicitly:

```php
foreach ($pool->drainErrors() as $error) {
    // ['kind' => ..., 'message_id' => ..., 'message' => ...]
}
```

**Limits** (enforced natively): payload ≤ 1 MiB per message, headers ≤ 64 KiB / 128 flat entries per message, batches ≤ 256 messages / 1 MiB cumulative payload, `timeout_ms` ≤ 86,400,000.

## 3. Scale

One worker profile multiplexes several subscriptions through a weighted-fair scheduler — a single `consumer()` handle drains every queue, and higher-weight subscriptions get a proportionally larger share:

```php
'workers' => [[
    'name' => 'default',
    'subscriptions' => [
        [
            'name' => 'critical', 'broker' => 'default', 'queue' => 'payments',
            'weight' => 8, 'priority_class' => 1, 'prefetch' => 8,
        ],
        [
            'name' => 'bulk', 'broker' => 'default', 'queue' => 'emails',
            'weight' => 2, 'priority_class' => 0, 'prefetch' => 64,
        ],
    ],
    'scheduler' => ['strategy' => 'weighted_fair'],
]],
```

- `priority_class` — higher numbers are served first (client-side scheduler state, nothing sent to the broker); `starvation_after` (seconds, default 30) raises an aged subscription's effective priority so it cannot be starved forever.
- `prefetch` accepts a plain integer, `['mode' => 'fixed', 'value' => N]`, or an adaptive controller that keeps a time-bounded buffer of ready work and adjusts with hysteresis: `['mode' => 'adaptive', 'initial' => 64, 'min' => 1, 'max' => 256, 'target_buffer_seconds' => 5]`. Adaptive prefetch requires acknowledgements (`early_ack`/`no_ack` must stay off).

Several worker profiles can live on the same pool; open one consumer per profile. Several brokers can be declared in `brokers` — each subscription pins its `broker` by name, and each broker recovers independently.

## 4. Operate

`stats()` returns one snapshot of all native counters and latency percentiles — call it from the same process, anywhere you would call a publish or consume:

```php
$stats = $pool->stats();

$stats['publishes_total'];        // publications accepted
$stats['confirmations_total'];    // broker confirms received
$stats['returns_total'];          // mandatory returns (unroutable)
$stats['publication_retries_total']; // replay retries across recovery
$stats['duplicates_total'];       // deliveries flagged as redeliveries
$stats['reconnects_total'];       // recovery events
$stats['publish_buffered'];       // publications parked in the replay buffer
$stats['confirmation_latency_p99']; // ms
```

The full field list is in the `stats()` stub docblock; metric semantics (including what counts as a duplicate) are in [Reliability — Measuring duplicates](reference.md#measuring-duplicates).

**Events** are drained synchronously on the PHP thread during publish/consume/flush/stats calls — no polling:

```php
$pool->onConnectionState(function (string $broker, string $state, int $generation): void {
    // $state: disconnected | connecting | ready | recovering | failed_permanent | closed
});

$pool->onBackpressure(function (string $broker, int $inFlight, int $capacity): void {
    // the bounded publish buffer is filling up
});
```

**Graceful shutdown** — close the consumer, then the pool. The pool also flushes buffered publications and closes consumer handles on garbage collection, so an explicit close is hygiene, not a requirement:

```php
$consumer->close();
$pool->close();
```

## Going further

- [Installation](reference.md#installation) — platforms, PIE/Homebrew/Docker, distribution model, upgrades
- [Reliability](reference.md#reliability) — the at-least-once contract, recovery, replay buffer semantics
- [Troubleshooting](reference.md#troubleshooting) — common errors and diagnosis
- [Development guide](development.md) — building the extension from source
- [Laravel queue driver](https://github.com/Goopil/php-rabbit-rs/blob/main/packages/laravel-queue/docs/getting-started.md) — the same engine behind Laravel's `Queue` API
