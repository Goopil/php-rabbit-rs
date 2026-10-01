# connection.blocked/unblocked: measured backpressure signal — design (2026-09-28)

Implements Goopil/php-rabbit-rs#251. Decided during the reliability hardening
wave (`docs/plans/2026-09-11-reliability-hardening.md`, backlog row
"connection.blocked/unblocked handling") as post-1.0 observability work.

## Context

When RabbitMQ hits a resource alarm (memory/disk watermark), the broker sends
`connection.blocked` / `connection.unblocked` and stops reading from
publishers until unblocked. Today the stall is bounded — publications still
respect confirm timeouts and per-publication deadlines — but it is measured
only through its consequences. Operators cannot distinguish "broker blocked"
from "slow broker".

Key finding: lapin already emits `Event::ConnectionBlocked(String)` /
`Event::ConnectionUnblocked` on the event stream the transport adapter
already subscribes to; `LapinErrorStream` currently discards every non-Error
event (`crates/rabbit-rs-core/src/transport/lapin.rs:90-92`). The signal
arrives and is dropped. This feature only wires it through.

## Resolved decisions

1. **Behavior: observability only.** Blocked/Unblocked events never change
   publish failure semantics. Confirm timeouts and publication deadlines
   remain the only failure bounds.
   - Short block (< deadlines): publications park in bounded replay memory
     and confirm once the broker unblocks. The stall is absorbed.
   - Prolonged block (> deadlines): the existing typed `Timeout` path fires,
     publications are re-buffered with the same `message_id` and original
     deadline until it expires, then resolve terminally to the caller.
     Nothing is silently lost.
2. **Plumbing: generalize the transport stream.** `TransportErrorStream`
   becomes `TransportEventStream` carrying `TransportEvent`. One stream, one
   subscription, one consumer arm in the connection actor.
3. **Persistence: out of scope.** Durability beyond a PHP process crash
   requires an external outbox (existing invariant). A DB-backed spool for
   undelivered publications is future work for the Laravel package; see
   Future work. The gauge added here is the hook an application (or that
   future feature) uses to detect backpressure proactively.

## Design

### 1. Transport surface (`crates/rabbit-rs-core/src/transport.rs`)

Rename `TransportErrorStream` → `TransportEventStream` and widen the yielded
item:

```rust
pub enum TransportEvent {
    /// Connection-fatal error (existing semantics; triggers recovery).
    Error(TransportError),
    /// Broker applied backpressure (resource alarm). Carries the
    /// broker-provided reason string.
    Blocked(String),
    /// Broker lifted backpressure.
    Unblocked,
}
```

`TransportConnection::error_stream()` → `event_stream()`. The stream
contract is unchanged: created once per connection, selected over by the
caller; `None` means the connection is gone.

### 2. Lapin adapter (`crates/rabbit-rs-core/src/transport/lapin.rs`)

`LapinErrorStream` → `LapinEventStream`. The discard branch becomes a match:

- `lapin::Event::Error(e)` → `TransportEvent::Error(...)` (unchanged
  classification via the existing `connection_alive` closure)
- `lapin::Event::ConnectionBlocked(reason)` → `TransportEvent::Blocked(reason)`
- `lapin::Event::ConnectionUnblocked` → `TransportEvent::Unblocked`
- `Connected` / `SendFlow` remain ignored.

### 3. Mock transport (`crates/rabbit-rs-core/src/transport/mock.rs`)

Scripting knobs mirroring `push_connection_error`:
`push_blocked(reason: &str)` and `push_unblocked()`, backed by the existing
shared-state notify machinery. The mock event stream yields
`TransportEvent::Blocked` / `Unblocked` when scripted and stays pending
otherwise.

### 4. Connection actor (`crates/rabbit-rs-core/src/pool/connection_actor.rs`)

`handle_ready` already selects over this stream. The match on the pulled
item routes:

- `TransportEvent::Error(e)` → existing path (`close_connection` +
  `route_loss`).
- `TransportEvent::Blocked(reason)` → record metrics + warn log, continue.
- `TransportEvent::Unblocked` → clear metrics + info log, continue.

Blocked/Unblocked never trigger recovery. When a connection is lost or
closed (including while blocked), the gauge resets to 0: a new connection
starts unblocked by definition. The counter is not reset.

### 5. Metrics (`crates/rabbit-rs-core/src/metrics.rs`)

Two new atomics on the existing lock-free registry:

- `connection_blocked_total: AtomicU64` — counter of blocked episodes
  (each `Blocked` event increments; not reset on recovery).
- `connection_blocked: AtomicU64` — gauge, 0 or 1: whether any current
  connection is blocked. First gauge in the registry; snapshot reads it
  directly (no histogram, no labels).

Both appear in `MetricsSnapshot` with doc comments. Multi-connection
semantics (a client owning several connections): the gauge is 1 when at
least one connection is blocked; the counter accumulates across
connections. This is documented on the snapshot fields.

Naming note: `backpressure_total` already exists and means in-process
buffer-ceiling refusals; the broker-level signal deliberately uses the
`connection_blocked*` names.

### 6. PHP surface (`crates/rabbit-rs-php/src/classes/pool.rs`)

`Pool::stats()` exposes the two snapshot fields under the same names:
`connection_blocked` (0/1) and `connection_blocked_total`. The docblock
return-shape contract (pool.rs:284-293) gains both keys. No other PHP API
changes: no callbacks, no exceptions, no new methods.

### 7. Logging (`crates/rabbit-rs-core/src/log.rs` facade)

- `Blocked` → `warn` on target `connection_actor`, message
  `broker blocked: {reason}` with the broker-provided reason truncated to
  200 characters (protocol-provided string; never credentials — the facade
  redaction contract still applies).
- `Unblocked` → `info` on the same target.
- Silent when no sink is installed (unchanged facade contract).

### Failure semantics (explicitly unchanged)

The feature adds no failure path. A blocked broker starves confirmations;
existing machinery handles both regimes: temporary blocks are absorbed by
bounded in-process replay (same `message_id`, original deadline), prolonged
blocks end in typed terminal `Timeout` at the original deadline. This is
the at-least-once contract: confirmed, returned, or surfaced as a typed
error — never silent.

## Testing

1. **Core integration test (mock transport)** — new file
   `crates/rabbit-rs-core/tests/connection_blocked.rs`:
   - `push_blocked("memory alarm ...")` → gauge = 1, counter = 1, warn log
     observed via a test sink.
   - `push_unblocked()` → gauge = 0, counter unchanged.
   - Second blocked episode → counter = 2.
   - Connection loss while blocked → gauge resets to 0, counter retained,
     recovery runs to a new Ready generation (existing liveness pattern).
2. **Existing publisher tests stay green**: confirm-timeout behavior under
   a pending confirmation is unaffected (deadlines remain the only bound).
3. **PHP Pest (extension suite)**: `Pool::stats()` contains the two keys
   with numeric values after a publish-healthy setup.
4. **Optional lab e2e (manual)**: `rabbitmqctl set_vm_memory_high_watermark
   0.05` on the lab broker produces real events; assert gauge/counter in
   `stats()`.

## Non-goals (YAGNI)

- No fast-fail of publications during blocked, no new `PublishErrorKind`.
- No blocked-duration histogram, no `unblocked_total` counter.
- No PHP-visible callback/event API.
- No outbox/persistence mechanism in the Rust core.

## Future work (tracked separately, not in this feature)

- **Laravel DB spool / external outbox**: optional package-level mode that
  persists undelivered publications in the application database until the
  broker is available again. The `connection_blocked` gauge is the signal
  such a mode would consume. Deserves its own issue and design.
