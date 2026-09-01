# Reliability

Rabbit RS provides at-least-once delivery. Silent loss is unacceptable; duplicates are permitted and must remain identifiable and measurable.

The documented exception is `publisher.safety = blind`: an explicit fire-and-forget mode (silent loss possible) that is configurable through the Laravel package (`publisher.safety`, env `RABBIT_RS_SAFETY`) as well as the raw native extension configuration — an explicit `blind` (or `unsafe`) value takes precedence over the legacy `confirms`/`mandatory` flags; see [Configuration](configuration.md).

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

Mandatory routing is **always on in safe mode** (`publisher.safety = "safe"`). When enabled:

- The broker returns unroutable messages via `basic.return` instead of silently dropping them
- `basic.return` is processed **before** the corresponding `basic.ack` — a return takes precedence over a following ACK
- The publish call resolves with a `Returned` outcome, and the Laravel bridge throws a `QueueException`

The legacy `publisher.mandatory = false` flag is **rejected at validation** with an actionable error: confirms without mandatory routing would let the broker silently drop unroutable messages, which breaks the no-silent-loss contract. The only supported opt-out is `publisher.safety = "unsafe"` or `"blind"`.

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

With multiple brokers, each broker recovers independently through its own coordinator: one broker recovering never blocks consumption from the others. When a broker's consumer set is replaced after recovery, the composed multi-broker consumer surfaces a one-shot `ConnectionException` ("broker source replaced by recovery; re-fetch consumer") — re-fetch the consumer to resume deliveries from that broker (see [Multiple brokers and vhosts](configuration.md#multiple-brokers-and-vhosts)).

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

The replay buffer is **bounded** by the publisher's global buffer capacity — a shared budget for in-flight confirms and replayed publications (1024 publications and 64 MiB of buffered payload bytes by default). When the budget is exhausted, new publications receive `Backpressure` instead of being accepted.

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

## Panic policy

Rabbit RS runs as a native PHP extension: an uncaught Rust unwind crossing the FFI boundary aborts the whole PHP process. The core therefore keeps panics out of every code path reachable from a PHP operation, and routes diagnostics through the log facade (`rabbit_rs_core::log`) instead of stderr.

1. Production code must not call `unwrap()`, `expect()`, or the `panic!` family on paths reachable from a PHP operation. Prefer typed errors with actionable context.
2. A remaining `expect`/`unwrap` is accepted only as a documented, proven invariant — one that cannot fire without a prior logic bug in the same synchronous block (see the audit below).
3. Panics in `#[cfg(test)]` code are out of scope; tests may panic freely.
4. Background Tokio tasks must terminate cleanly on failure (log through the facade, then return) instead of panicking inside a spawned task.

## Log facade

- The core depends on no logging framework and never writes to stderr.
- Embedders install one process-wide sink (`rabbit_rs_core::log::install`); the first installation wins and later calls are rejected, which keeps forks and repeated initializations deterministic.
- Without an installed sink the core is silent; records emitted before the first install are dropped, so install at startup before spawning pools.
- Redaction contract: call sites only log broker names, connection generations, and transport error messages — never credentials, complete broker URIs, or private certificate material. Sinks must preserve this when forwarding.

## Panic audit (2026-09-01, issue #56)

`rg -n 'unwrap\(\)|expect\(|panic!|unreachable!|todo!|unimplemented!'` over `crates/rabbit-rs-core/src` and `crates/rabbit-rs-php/src`, restricted to production code (`#[cfg(test)]` modules excluded).

### Fixed in this round

| Site | Problem | Resolution |
| --- | --- | --- |
| `pool/recovery_coordinator.rs` `wait_for_state` | `expect` on the state watch when the coordinator task had stopped: panic reachable from PHP-facing waits | Returns `ConnectionState::Closed` when the watch dies; `state()` also reports `Closed` for a dead watch so pool loops observe a terminal state |
| `pool/recovery_coordinator.rs` `run_coordinator` | `expect("connection actor started")` inside a spawned task | Logs through the facade (`Level::Error`) and terminates the task cleanly |
| `pool/recovery_coordinator.rs` recovery failure | `eprintln!` leaked diagnostics to stderr | Routed through the log facade (`Level::Warn`), carrying the typed `CoordinatorError` |

### Accepted invariants (documented, no runtime conversion)

| Site | Invariant |
| --- | --- |
| `client.rs` `topology_plan()` fallback `.expect("external mode always compiles")` | `TopologyPlan::compile` on an empty `External`-mode plan validates nothing and cannot fail; the expect guards a compile-time-true property |
| `consumer/actor.rs` `drain_pending` `.expect("front checked above")` | `pop_front` runs only after `front()` returned `Some` in the same synchronous block with no mutation in between |
| `consumer/attempts.rs` `DEFAULT_MAX_ATTEMPTS_NON_ZERO` | Const-evaluated `match`; the `panic!` fires at compile time if the constant is ever zero, never at runtime |
| `topology/delay.rs` `write!(...).expect("writing to String is infallible")` | `fmt::Write for String` cannot fail |

### PHP extension (`crates/rabbit-rs-php`)

`callbacks.rs` (callback registry), `classes/bridge.rs`, and `classes/publish_buffer.rs` call `.expect("... mutex poisoned")` on `std::sync::Mutex` locks. A poisoned mutex requires a prior panic while the lock was held; the critical sections in these types perform no panicking operations, so the poison state is unreachable in practice. These sites stay documented rather than converted: converting them would trade a proven invariant for error propagation through FFI paths that have no meaningful recovery.

## Error typing

`CoordinatorError` is a typed enum (`Topology`/`Transport`/`Publisher`/`Consumer`/`Internal`) whose variants carry the typed source error; `Display` messages keep the previously surfaced context. Callers must classify through variants, never through string matching.
