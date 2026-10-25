# Migrating from php-amqplib

This guide is for applications that talk to RabbitMQ directly through
[php-amqplib/php-amqplib](https://github.com/php-amqplib/php-amqplib) and want to move
to the `rabbit_rs` native extension. It maps the php-amqplib API you use today to the
`Goopil\RabbitRs\Pool` API and states the semantic differences that need a decision
from you, not just a rename.

For the Laravel path, see [Migrating from laravel-queue-rabbitmq](migration-from-laravel-queue-rabbitmq.md).

> **Verify against your version.** The php-amqplib signatures below were checked
> against the upstream `master` branch (3.x line) at the time of writing. Method and
> callback names have moved between releases — the basic-return callback, for
> example, is registered with `set_return_listener()` on master but was registered
> differently in older majors. Verify against the version installed in your
> `composer.lock` before copying code.

The authoritative rabbit_rs API is the generated stub:
[`crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`](https://github.com/Goopil/php-rabbit-rs/blob/main/crates/rabbit-rs-php/stubs/rabbit_rs.stub.php).

## 1. Install

```bash
pie install goopil/rabbit-rs-native
php --ri rabbit_rs   # verify it loads
```

Requirements and alternative distribution channels (Homebrew, Docker): [Installation](reference.md#installation).

## 2. API mapping

| php-amqplib | rabbit_rs |
| ----------- | --------- |
| `new AMQPStreamConnection($host, $port, $user, $password, $vhost)` | `new Pool($config)` — brokers, credentials, TLS, heartbeat are one config array |
| `AMQPStreamConnection::create_connection($hosts, $options)` (host failover list) | multiple endpoints in the broker's `hosts` list |
| `$connection->channel()` | no explicit channel objects; the pool manages channels and multiplexing |
| `$channel->queue_declare($queue, $passive, $durable, $exclusive, $auto_delete, $nowait, $arguments)` | `'topology_mode' => 'declare'` in the config, or explicit `Pool::declareQueue()` |
| `$channel->exchange_declare(...)` / `$channel->queue_bind(...)` | `topology_mode` plus `Pool::bindQueue()`; `Pool::verifyExchange()` for a passive check |
| `new AMQPMessage($body, $properties)` + `$channel->basic_publish($msg, $exchange, $routing_key, $mandatory)` | `Pool::publish([...])` with `'exchange'`, `'routing_key'` |
| `$channel->confirm_select()` + `set_ack_handler()` / `set_nack_handler()` / `set_return_listener()` + `$channel->wait_for_pending_acks_returns($timeout)` | built in: `safe` mode confirms and tracks routing by default; `flush()` is the confirm barrier |
| `$channel->batch_basic_publish($msg, ...)` + `$channel->publish_batch()` | `Pool::publishBatch([...])` (one boundary crossing, ≤ 256 messages) |
| `$channel->basic_qos($prefetch_size, $prefetch_count, $a_global)` | `'prefetch'` in the subscription config |
| `$channel->basic_consume($queue, $consumer_tag, $no_local, $no_ack, $exclusive, $nowait, $callback)` + `$channel->wait()` | `$pool->consumer($profile)` + `$consumer->next($timeoutMs)` (pull) |
| `$message->ack()` / `$channel->basic_ack($delivery_tag)` | `$delivery->ack()` |
| `$message->nack($requeue, $multiple)` / `$channel->basic_nack($delivery_tag, $multiple, $requeue)` | `$delivery->reject($requeue)` (AMQP `basic.reject`) |
| `$channel->basic_reject($delivery_tag, $requeue)` | `$delivery->reject($requeue)` |
| `$channel->basic_get($queue, $no_ack)` | `Pool::getMessage($broker, $queue, $requeue)` — settles immediately, returns `null` when empty |
| heartbeat: `PCNTLHeartbeatSender` or stream timeouts | built in: `'heartbeat' => N` (seconds) in the broker config |
| recovery: your own try/catch/reconnect loop (`AMQPConnectionClosedException`, `AMQPIOException`, …) | built in: deterministic automatic recovery, see [Auto-recovery](#semantic-differences) |

One structural difference runs through the whole table: php-amqplib exposes the AMQP
primitives (connections, channels, frames) and you assemble the guarantees yourself.
`rabbit_rs` exposes a pool whose guarantees — confirms, mandatory routing, recovery,
bounded buffers — are configured, with `'safety' => 'safe'` as the default.

## 3. Connect

php-amqplib:

```php
use PhpAmqpLib\Connection\AMQPStreamConnection;

$connection = new AMQPStreamConnection('127.0.0.1', 5672, 'guest', 'guest', '/');
$channel = $connection->channel();
```

rabbit_rs:

```php
use Goopil\RabbitRs\Pool;

$pool = new Pool([
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
            'prefetch' => 16,
        ]],
        'scheduler' => ['strategy' => 'weighted_fair'],
    ]],
    'topology_mode' => 'declare',
]);
```

The consumer profile (the `workers` entry) replaces the per-consumer `basic_consume`
setup: queues, weights, and prefetch are declared up front. One profile can subscribe
to several queues through one weighted-fair scheduler; each subscription pins its
broker by name, and every broker recovers independently.

Where php-amqplib's `create_connection()` gives you a failover list of hosts, the
broker's `hosts` list does the same inside one broker entry. A distinct vhost stays a
distinct AMQP connection.

## 4. Publish with confirms

php-amqplib (the wiring the confirm extension requires):

```php
use PhpAmqpLib\Message\AMQPMessage;

$msg = new AMQPMessage(json_encode(['hello' => 'world']), [
    'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
    'message_id' => uniqid('', true),
]);

$channel->set_ack_handler(function (AMQPMessage $message): void {
    // confirmed
});
$channel->set_nack_handler(function (AMQPMessage $message): void {
    // broker rejected the publication
});
$channel->set_return_listener(function (int $replyCode, string $replyText, string $exchange, string $routingKey, AMQPMessage $message): void {
    // unroutable (mandatory) — callback shape: verify against your version
});
$channel->confirm_select();

$channel->basic_publish($msg, '', 'hello', mandatory: true);
$channel->wait_for_pending_acks_returns(5.0);
```

rabbit_rs:

```php
$pool->publish([
    'broker' => 'default',
    'exchange' => '',
    'routing_key' => 'hello',
    'payload' => json_encode(['hello' => 'world']),
    'message_id' => uniqid('', true),
    'timeout_ms' => 5000,
]);
$pool->flush(); // barrier: every publish above is confirmed, returned, or timed out when this returns
```

Everything the handler trio did is folded into the default `safe` mode: confirms and
mandatory routing are on, and an unroutable message comes back as a `returns_total`
outcome instead of being dropped. Publishing is asynchronous internally; `flush()`
plays the role of `wait_for_pending_acks_returns()`. Outcomes that could not be
delivered synchronously (a confirm timeout during a recovery window, for instance)
surface at the next operation, or explicitly:

```php
foreach ($pool->drainErrors() as $error) {
    // ['kind' => ..., 'message_id' => ..., 'message' => ...]
}
```

Batch publishing: `Pool::publishBatch(array $messages)` crosses the PHP/Rust boundary
once for up to 256 messages (1 MiB cumulative payload) — the counterpart of
`batch_basic_publish()` + `publish_batch()`, with outcomes tracked per message.

## 5. Consume

php-amqplib (push model — the library calls your callback from `wait()`):

```php
$channel->basic_qos(0, 16, false);
$channel->basic_consume('hello', '', false, false, false, false, function (AMQPMessage $message): void {
    echo $message->getBody(), PHP_EOL;
    $message->ack();
});

while ($channel->is_consuming()) {
    $channel->wait();
}
```

rabbit_rs (pull model — you ask for the next delivery):

```php
$consumer = $pool->consumer('default'); // the worker profile name

while (true) {
    $delivery = $consumer->next(1000); // ms; null on timeout
    if ($delivery === null) {
        continue;
    }

    echo $delivery->payload(), PHP_EOL;
    $delivery->ack();
}
```

- The prefetch from the subscription config replaces `basic_qos()`.
- `$delivery->metadata()` carries `message_id`, `correlation_id` (when present),
  `subscription`, `attempts`, and headers — the equivalent of reading
  `$message->get('message_id')` and friends.
- `$consumer->nextBatch($max, $timeoutMs)` and `tryNext()` cover batch and non-blocking
  pulls; `ackBatch()` (≤ 256 deliveries) and `ackThrough()` cover group settlement.
- `getMessage()` on the pool is a `basic.get` equivalent, but it settles the message
  immediately (`$requeue = true` inspects without consuming), so it is not a
  replacement for a consume loop.

## 6. Ack and nack

| php-amqplib | rabbit_rs | Notes |
| ----------- | --------- | ----- |
| `$message->ack()` / `$channel->basic_ack($delivery_tag)` | `$delivery->ack()` | fire-and-forget with bounded backpressure |
| `$message->nack($requeue = false, $multiple = false)` | `$delivery->reject($requeue)` | `reject(true)` = requeue, `reject(false)` = terminal (dead-letter exchange when one is bound) |
| `$channel->basic_nack($delivery_tag, $multiple = false, $requeue = false)` | `$delivery->reject($requeue)` | argument order differs between the php-amqplib helpers — mind the port |
| — | `$delivery->release($delayMs = 0)` | requeue now or after a delay |
| — | `$consumer->ackBatch($deliveries)` / `ackThrough($delivery)` | one `basic.ack` with `multiple=true`, or a bounded batch across channels |

php-amqplib's nack carries both `multiple` and `requeue`; `rabbit_rs` splits the
roles: `reject()` for the requeue-or-terminal decision, `ackThrough()` for the
`multiple=true` case, `release()` for delayed requeueing.

## Semantic differences

### At-least-once delivery

The contract is unchanged in kind (it is what you already implement around
php-amqplib) but explicit and enforced: once a message is accepted into the confirmed
delivery path, silent loss is unacceptable; duplicates are permitted, identifiable,
and measurable. A worker that crashes between receiving and acknowledging causes a
redelivery, so your processing must be idempotent. The `duplicates_total` counter in
`stats()` counts deliveries flagged as redeliveries, so you can measure the duplicate
rate instead of inferring it.

### Publisher confirms on by default

php-amqplib makes confirms opt-in: `confirm_select()`, three handlers, and a
`wait_for_pending_acks_returns()` call per batch. rabbit_rs defaults to `safety =
safe`: publisher confirms plus mandatory routing, every publish resolved to
confirmed, returned, or timed out. There is no separate `mandatory` flag — it follows
the safety mode. The opt-outs are `'safety' => 'unsafe'` (a synchronous socket write
with no outcome tracking; unroutable messages are silently dropped) and
`'safety' => 'blind'` (fire-and-forget through a bounded background pump; a transport
failure after hand-off is a silent loss). If your php-amqplib code publishes without
confirms today, `safe` is a strict upgrade; if you deliberately chose unconfirmed
publishing for throughput, set the mode explicitly.

### `message_id` dedup visibility

`publish()` requires a `message_id` and returns it. It is the stable identity of the
message: recovery replay re-uses the same id, `stats()['duplicates_total']` counts
redelivered deliveries, and `drainErrors()` records carry the id. Your consumer can
dedup on `$delivery->metadata()['message_id']` — the same key your publisher chose.
php-amqplib leaves `message_id` as one optional AMQP property among many; here it is
the dedup contract.

### Auto-recovery behavior

php-amqplib documents connection recovery as an application-level pattern: catch
`AMQPConnectionClosedException` (and friends), clean up, rebuild the connection and
channels, and re-run your setup — with the README's explicit warning that producers
need extra code to avoid publishing the same message twice.

rabbit_rs does this internally: the pool recovers automatically in a deterministic
order (connection, channels, exchanges, queues, bindings, QoS, consumers), and
publications that were in flight when the connection died stay in a bounded replay
buffer and are re-published with their original `message_id` and original deadline.
A publication whose deadline expired while parked during a recovery suspension is
re-armed once with a fresh deadline before failing terminally — retries are visible
in `publication_retries_total`.

Two boundaries to know:

- The replay buffer is **process memory, not durability**. It survives connection
  failures inside the same PHP process; it does not survive a PHP process crash.
  Durability across a crash needs an external outbox.
- The publish buffer is bounded. Under an outage with sustained traffic,
  `publish()` throws `BackpressureException` (retry the same message later);
  already-buffered messages are never dropped. Connection-level transport failures
  throw `Goopil\RabbitRs\ConnectionException`.

Limits enforced natively: payload ≤ 1 MiB per message, headers ≤ 64 KiB / 128 flat
entries, batches ≤ 256 messages / 1 MiB cumulative payload, `timeout_ms` ≤ 86,400,000.

### Other adjustments

- **No channels in userland.** Consumer channels are dedicated per subscription;
  publisher channels are pooled. Queue/exchange management goes through the pool
  methods or `topology_mode`.
- **Push → pull.** The consume loop is `next()`-driven, which makes signals, timeouts,
  and interleave-with-other-work straightforward; there is no `wait()` re-entrancy to
  manage.
- **Events instead of ad-hoc logging.** `onConnectionState()` and `onBackpressure()`
  deliver state transitions synchronously on the PHP thread.
- **Metrics.** `stats()` exposes publishes, confirmations, returns, duplicates,
  reconnects, buffered publications, and latency percentiles in one snapshot.

Full contract: [Reliability](reference.md#reliability). Full API: the
[stub](https://github.com/Goopil/php-rabbit-rs/blob/main/crates/rabbit-rs-php/stubs/rabbit_rs.stub.php)
and [Getting started](getting-started.md).

## 15-minute switch checklist

1. **Install (2 min)** — `pie install goopil/rabbit-rs-native && php --ri rabbit_rs`.
2. **Translate the connection (2 min)** — host/port/username/password/vhost/TLS into
   the `brokers` entry of the `Pool` config; add the consumer profile for the queues
   you consume.
3. **Decide the topology mode (2 min)** — `'declare'` to create queues on first use,
   `'verify'` for an externally provisioned topology, `'external'` to never touch it.
4. **Port the publisher (3 min)** — replace `basic_publish` with `Pool::publish()`
   (keep or introduce a stable `message_id`), and replace
   `wait_for_pending_acks_returns()` calls with `flush()` at the same boundary.
5. **Port the consumer (3 min)** — replace `basic_consume` + `wait()` with
   `consumer()->next()`; `ack()` → `ack()`, nack-with-requeue → `reject(true)`,
   terminal reject → `reject(false)`.
6. **Smoke-test (2 min)** — publish one message, consume it, then check
   `stats()`: `publishes_total` is accounted for by `confirmations_total` and
   `returns_total` (nothing left buffered or timed out).
7. **Cut over (1 min)** — remove `php-amqplib/php-amqplib` from `composer.json` and
   run your suite.
