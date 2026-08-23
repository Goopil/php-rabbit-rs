# Reliability

Rabbit RS provides at-least-once delivery. Silent loss is unacceptable; duplicates are permitted and must remain identifiable and measurable.

## At-least-once contract

The delivery contract is:

- **No silent loss** — every published message is either confirmed or the caller is notified of failure
- **Duplicates permitted** — in failure windows, the same message may be delivered more than once
- **Duplicates identifiable** — each message carries a stable `message_id` (UUID from Laravel payload)
- **Duplicates measurable** — metrics track redeliveries and duplicate counts

This means your jobs **must be idempotent**. Use the `message_id` to detect and handle duplicates at the application level.

## Publisher confirms

Publisher confirms are **enabled by default** (`publisher.confirms = true`). When enabled:

1. The publisher calls `confirm.select` on the channel
2. Each published message is assigned a sequence number
3. The broker sends `basic.ack` (confirmed) or `basic.nack` (rejected) with the sequence number
4. The publish call resolves only after the confirm is received

A confirm timeout (`publisher.confirm_timeout`, default 30 seconds) ensures the call does not hang indefinitely.

## Mandatory returns

Mandatory routing is **enabled by default** (`publisher.mandatory = true`). When enabled:

- The broker returns unroutable messages via `basic.return` instead of silently dropping them
- `basic.return` is processed **before** the corresponding `basic.ack` — a return takes precedence over a following ACK
- The publish call resolves with a `Returned` outcome, and the Laravel bridge throws a `QueueException`

Without mandatory routing, messages published to a non-existent queue or binding are silently lost. Rabbit RS enables this by default to enforce the no-silent-loss contract.

## Connection recovery

Rabbit RS handles connection loss automatically. The connection state machine is:

```
Disconnected → Connecting → Ready → Recovering → Ready
                                    |
                                    +→ Draining → Closed
```

### Recovery sequence

Recovery follows a **deterministic order**:

1. **Connection** — re-establish TCP connection and AMQP negotiation
2. **Channels** — open new publisher and consumer channels
3. **Exchanges** — declare or verify exchanges
4. **Queues** — declare or verify queues
5. **Bindings** — declare or verify bindings
6. **QoS** — re-apply prefetch settings
7. **Consumers** — re-register `basic.consume` for each subscription
8. **Publisher replay** — replay unconfirmed publications from the bounded buffer

This order ensures that consumers are only re-registered after their queues and bindings exist, and that publishers only resume after the topology is restored.

### Backoff

Retries use exponential backoff with jitter:

- Initial backoff: 100 ms
- Multiplier: 2x
- Maximum: 30 seconds
- Jitter: 20%

Permanent errors (authentication failures, incompatible topology) are not retried in publish contexts. Consumer workers may continue retrying according to their own policy.

## Delivery tokens and stale ACK rejection

Each delivery carries an opaque token containing:

- Connection identity
- Channel ID
- Consumer tag
- Delivery tag
- **Connection generation**

After a connection recovery, the generation increments. If the PHP code attempts to ACK a delivery from an old generation, the extension **rejects the stale ACK**. RabbitMQ redelivers the message.

This handles the race condition where:
1. A job is delivered to PHP
2. The job completes, but the ACK hasn't reached the broker
3. The connection drops
4. The connection recovers (new generation)
5. PHP attempts to ACK the old delivery
6. The extension rejects the stale ACK
7. RabbitMQ redelivers the message

The job may be executed twice. This is expected and why jobs must be idempotent.

## Replay buffer

When a connection drops before a publish is confirmed, the state is ambiguous — the broker may or may not have received the message. Rabbit RS handles this by:

1. **Classifying unconfirmed publications as ambiguous** — the publish call does not resolve immediately
2. **Placing them in a bounded in-memory replay buffer** — with the same `message_id`, payload, destination, and original deadline
3. **Replaying them after recovery** — once the topology is restored and a new confirm-enabled channel is open
4. **Reusing the original deadline** — the deadline is never reset by a reconnection

The replay buffer is **bounded** by the publisher's global capacity (`max_in_flight` plus in-flight confirms). When capacity is reached, new publications receive `Backpressure` instead of being accepted.

### What the replay buffer is not

The replay buffer is **in-memory only**. It survives connection drops but **not** a PHP process crash. If the PHP process crashes, all unconfirmed publications in the buffer are lost.

For durability beyond a process crash, use an **external outbox** pattern:

1. Write the job to a persistent store (database) within the same transaction as your business operation
2. A separate process reads from the outbox and publishes to RabbitMQ
3. Delete the outbox entry after a publisher confirm

Rabbit RS does not include an outbox in V1. The in-memory replay buffer covers the common case of transient network failures.

## Duplicates

Duplicates are expected and normal. They occur in these scenarios:

| Scenario | Cause |
|----------|-------|
| Connection drop after delivery, before ACK | Stale ACK rejected, RabbitMQ redelivers |
| Connection drop after publish, before confirm | Replay buffer republicates; broker may have received the original |
| Worker crash with in-flight jobs | RabbitMQ redelivers unacked messages |

### Handling duplicates

1. **Make jobs idempotent** — use `message_id` or business keys to detect duplicate work
2. **Use `attempts()`** — the `RabbitMqJob::attempts()` method returns the delivery count from `x-acquired-count` or `x-delivery-count` headers
3. **Set `delivery_limit`** — quorum queues dead-letter messages that exceed the limit; `dead_letter` must be configured when `delivery_limit` is set
4. **Monitor duplicates** — track `reconnects_total` and `deliveries_total` metrics; spikes indicate recovery-induced duplicates

### Measuring duplicates

The status command shows metrics:

```bash
php artisan rabbit-rs:status
```

Key metrics:
- `reconnects_total` — number of connection recoveries (each can cause duplicates)
- `deliveries_total` — total deliveries received
- `acks_total` / `rejects_total` — settlement counts

## When to use an external outbox

Use an external outbox when:

- You need durability across PHP process crashes (not just connection drops)
- You publish within a database transaction and need the publish to be transactional with the database write
- You cannot tolerate any message loss, even in the ambiguous window

Without an outbox, the in-memory replay buffer covers transient network failures but not process crashes. For most Laravel applications, the default behavior is sufficient — PHP workers are typically supervised by Supervisor or Kubernetes and restart automatically.
