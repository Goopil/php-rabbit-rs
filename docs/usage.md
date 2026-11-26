# Using the PHP extension

The `rabbit_rs` extension works without Laravel. This guide covers publishing and consuming messages in a plain PHP application. Start with the [getting-started guide](getting-started.md) to install the extension and create the `$pool` used in these examples.

- [Publish a message](#publish-a-message)
- [Choose a publishing mode](#choose-a-publishing-mode)
- [Publish a batch](#publish-a-batch)
- [Consume and settle messages](#consume-and-settle-messages)
- [Handle failures and recovery](#handle-failures-and-recovery)
- [Inspect the pool](#inspect-the-pool)
- [Close resources](#close-resources)

## Publish a message

A pool uses the broker and worker profiles from its configuration. The [first-message example](getting-started.md#1-hello-world) creates a `default` broker and an `hello` queue. Publish to that queue through RabbitMQ's default exchange:

```php
$messageId = bin2hex(random_bytes(16));

$pool->publish([
    'broker' => 'default',
    'exchange' => '',
    'routing_key' => 'hello',
    'payload' => json_encode(['order_id' => 42], JSON_THROW_ON_ERROR),
    'message_id' => $messageId,
    'timeout_ms' => 5000,
]);

$pool->flush();
```

The empty exchange routes to a queue whose name matches the routing key. In the getting-started configuration, declare mode creates the queue. For an externally managed topology, provision it yourself and choose the appropriate topology mode.

`publish()` returns the message ID, but a return from that call alone does not mean the broker confirmed the message: publications can be buffered. In the default `safe` mode, `flush()` waits for buffered publications to be confirmed or raises a failure. Use a stable message ID for a logical message and reuse it when retrying that same message; generating a fresh ID for every retry prevents ID-based deduplication.

## Choose a publishing mode

Set `publisher.safety` in the configuration passed to `new Pool()`:

```php
'publisher' => [
    'safety' => 'safe', // 'safe', 'unsafe', or 'blind'
],
```

- `safe` uses publisher confirms and mandatory routing for ordinary publishes. Recovery can replay an unconfirmed message, so consumers must tolerate duplicates.
- `unsafe` uses neither confirms nor mandatory returns. An unroutable publication or a later transport failure can be lost silently.
- `blind` hands work to a bounded background pump without waiting for a broker outcome. Loss is acceptable by contract. Delayed publications use the configured delay routing but still have no confirmed outcome.

In `blind` mode `flush()` is a pump hand-off barrier, not a broker confirmation; work may also be dropped during recovery. The detailed comparison, an idempotency example, and mixed-workload guidance are in [Choosing a delivery mode](../packages/laravel-queue/docs/delivery-modes.md). Delayed-plugin publishes have a [mandatory-routing exception](reference.md#delayed-publishes).

## Publish a batch

Use `publishBatch()` when you already have several messages ready. The call accepts at most 256 messages and 1 MiB of cumulative payload; header limits also apply across the call.

```php
$messages = [];

foreach ([42, 43] as $orderId) {
    $messages[] = [
        'broker' => 'default',
        'exchange' => '',
        'routing_key' => 'hello',
        'payload' => json_encode(['order_id' => $orderId], JSON_THROW_ON_ERROR),
        'message_id' => bin2hex(random_bytes(16)),
        'timeout_ms' => 5000,
    ];
}

$messageIds = $pool->publishBatch($messages);
```

In `safe` mode the call awaits batch outcomes. A batch failure may be partial: some messages may already have reached the broker even when the call throws. Retrying the batch can therefore produce duplicates; keep the original message IDs and make processing idempotent. In `blind` mode there is no broker outcome to await. See the [`publishBatch()` API](../crates/rabbit-rs-php/stubs/rabbit_rs.stub.php) for limits and error details.

## Consume and settle messages

Open the worker profile named in your pool configuration. `next()` waits up to the supplied number of milliseconds and returns `null` when no message is available:

```php
$consumer = $pool->consumer('default');

while (true) {
    $delivery = $consumer->next(1000);

    if ($delivery === null) {
        continue;
    }

    $metadata = $delivery->metadata();
    $payload = $delivery->payload();

    // Apply your idempotent business operation using $metadata['message_id'].

    $delivery->ack();
}
```

Acknowledge only after processing succeeds. `ack()` enqueues settlement; it is not a broker acknowledgement of your acknowledgement. A connection failure before settlement reaches RabbitMQ can cause redelivery.

When processing cannot succeed, `$delivery->reject(true)` requests requeue and `$delivery->reject(false)` rejects without requeue; a broker-configured dead-letter exchange can route terminally rejected messages. `$delivery->release($delayMs)` releases a delivery with an optional delay. See the [delivery API](../crates/rabbit-rs-php/stubs/rabbit_rs.stub.php) and [reliability reference](reference.md#delivery-tokens-and-stale-ack-rejection) for settlement behavior.

## Handle failures and recovery

In `safe` mode the pool retains unconfirmed publications in a bounded in-process buffer and replays them during connection recovery with the same message ID. A PHP process crash loses that buffer; use an external outbox when messages must survive a crash before broker confirmation.

`BackpressureException` means the bounded publish buffer is full. Keep the message and its ID so your application can retry it according to its policy. Connection-level failures can surface as `ConnectionException`; a consumer source replacement can require fetching the consumer again with `$pool->consumer($profile)`. Permanent configuration or topology errors need operator action rather than an endless retry loop.

Publish failures from a pipelined drain can surface on a later pool operation. You can instead inspect and clear pending publish error records explicitly:

```php
foreach ($pool->drainErrors() as $error) {
    // Record $error['kind'], $error['message_id'], and $error['message'].
}
```

`drainErrors()` clears the records it returns, so handle each result. Consumer settlement errors have their own `$consumer->drainErrors()` method. See [Connection recovery](reference.md#connection-recovery), [replay buffer](reference.md#replay-buffer), and [error typing](reference.md#error-typing).

## Inspect the pool

Read a snapshot in the PHP process that owns the pool:

```php
$stats = $pool->stats();

$stats['confirmations_total'];
$stats['returns_total'];
$stats['reconnects_total'];
$stats['duplicates_total'];
```

These are process-local counters. `duplicates_total` counts broker-flagged redeliveries settled by that process, not every duplicate publication. See [Measuring duplicates](reference.md#measuring-duplicates) for the limits and broker-side signals.

## Close resources

When stopping a long-lived process, close the consumer and then the pool:

```php
$consumer->close();
$pool->close();
```

Closing provides an opportunity to drain work and observe errors, but cannot make the in-memory publish buffer durable across a process crash. For the available methods and signatures, see the [generated PHP stub](../crates/rabbit-rs-php/stubs/rabbit_rs.stub.php).
