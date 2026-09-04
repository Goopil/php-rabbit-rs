# Pipelined safe flush — design note (Round D Phase 2, issue #41)

Date: 2026-09-04. Worktree `task/41-round-d`, building on the Phase 1 profiling
report (`.superpowers/round-d/README.md`, worktree `@ b1b6baa`).

## 1. Current mechanism and why it is slow (measured)

The PHP-side `PublishBuffer` batches publications and flushes when either
trigger fires (64-message threshold or 1 ms interval since the oldest buffered
publication, issue #96). The flush is a **barrier**:
`PublishBuffer::flush_batch` runs `block_on(client.publish_batch(requests))`,
so the PHP thread parks until every publisher confirm of the batch resolves.

Phase 1 measurements (3-node RabbitMQ 4.2.9 lab, release cdylib, official
3×10 protocol):

| observation | value |
|---|---|
| safe dispatch cell (baseline) | 5 740 msg/s (archive: 6 534) |
| barrier share of safe-mode wall time | **74 %** |
| batch size p50 (1 ms × ~24 k/s produce rate) | 24–25 msgs |
| `block_on` per flush | ≈ 2.0 ms + 36 µs × N |
| confirms sustained (barrier removed, prototype) | ~21 k/s, p50 RTT 3 ms |
| barrier removal priced (`RS_ASYNC_FLUSH`) | safe 5.3 k → 21.5 k msg/s (**×4.05**) |
| blind ceiling / produce ceiling | 21.0–21.9 k msg/s |

The transport API already supports pipelining (`publish` returns a
separately-awaitable confirmation, `transport.rs:389`); the actor releases all
messages before awaiting outcomes (`publish_batch` → `try_publish` →
`wait_all`). Only the PHP-side barrier serializes. Measured dead ends (Phase
1): more tokio workers (×1.0), deeper flush windows as the primary fix
(×2, still 3.3× short), property-clone surgery (≤ 5 µs/msg of a 36 µs
marginal).

## 2. The pipelined flush

`Pool::publish` keeps its exact current flow (validate → overflow check →
enqueue → trigger check), but the **threshold/interval auto-flush spawns the
batch on the runtime instead of blocking the PHP thread**. `push()` returns the
`message_id` before the batch is even handed to the actor — the same
fire-and-forget shape the transport API offers, with the safe-mode outcome
contract preserved asynchronously (§3).

The spawned drain task runs the existing `client.publish_batch(requests)`
(#83 contract intact: every already-accepted publication resolves before the
first terminal failure) and processes outcomes:

- `Confirmed` — dropped (the only outcome blind mode ever surfaced).
- `Returned` (mandatory, unroutable) — recorded in the buffer's **pending
  error queue** for PHP to surface; never re-buffered (definitive).
- batch `Err` — `Closed` (or a closed pool): counted in
  `dropped_publications_total`; otherwise the batch is re-buffered
  (deadline-expired publications dropped and counted, same as the current
  `rebuffer`) and a pending error is recorded. Retries happen at subsequent
  flushes exactly as today; duplicates are permitted and identifiable via
  `message_id`.

**What stays synchronous** (full-deadline semantics, called by the #87/#96
flush-barrier logic):

- explicit `flush()` — including `publishBatch`'s leading flush;
- `size()` / `clear()` flush barriers;
- the consumer-side `flush_nonempty()` before a pop (a publication accepted
  before a pop must be delivered before the pop blocks — #96/#100 semantics);
- `flush_teardown()` (destructor, fixed budget).

These four keep `block_on(publish_batch)` so their documented contracts are
byte-identical.

### Quiesce

Outstanding spawned drains are tracked (bounded set of `JoinHandle`s).
`flush()`, `close()`, and `__destruct` first **quiesce**: wait for every
spawned drain to complete, bounded by the existing `TEARDOWN_FLUSH_BUDGET`
(500 ms) as an overall deadline. After quiescing, the drain's re-buffered
publications are visible to the sync flush that follows, so graceful shutdown
does not strand buffered messages:

- `flush()`: quiesce (≤ 500 ms) → full-deadline sync drain.
- `close()`: quiesce (≤ 500 ms) → `flush()` → `client.close()`.
- `__destruct()`: quiesce (≤ 500 ms) → `flush_teardown()` (≤ 500 ms).

A drain that misses the quiesce budget keeps running detached on the
process-local runtime (the runtime registry outlives the pool). It still
finishes its outcome processing; on completion it re-buffers into the buffer
or — if the buffer is past teardown (`close()`/`__destruct` already ran, the
drain missed the budget and nobody will flush again) — counts the
publications in `dropped_publications_total` instead of re-buffering them
silently. The count may land after `stats()` was last observable; the
accounting is still explicit, matching the F-18 teardown-drop semantics.

### Bounded intake and backpressure

- PHP intake bounds unchanged: `PUBLISH_BUFFER_MAX_MESSAGES` (4096) /
  `PUBLISH_BUFFER_MAX_BYTES` (64 MB); `would_overflow` → best-effort flush →
  `BackpressureException`. Re-buffered publications may exceed the ceiling
  (already accepted, never silently dropped) — unchanged.
- Confirm-window bounds unchanged and measured working: publisher semaphore
  (`buffer_capacity`, 1024) + byte budget; a spawn that exceeds them fails
  `try_publish` with `Backpressure`, and the drain re-buffers the whole
  batch (conservative superset, #83 contract), pushing back on the producer.
- Concurrent spawned drains are explicitly capped (semaphore, 8). When the
  cap is hit the flushing `publish()` blocks until a drain completes —
  backpressure returns to the producer instead of unbounded task pile-up.
  Steady state at the measured ceiling holds ~1–3 concurrent drains, so the
  cap never binds in normal operation. The `onBackpressure` callback and
  `backpressure_total` metric keep working unchanged (they already observe
  the actor-side counters).

### Memory bound

Total in-process publication memory stays bounded by
buffer(≤ 4096 msgs) + spawned-drain batches (≤ 8 × one drained batch each,
each ≤ 4096 msgs) + actor windows (semaphore 1024 + byte budget 64 MB).
Every component is explicitly bounded; the byte budget is shared.

## 3. Outcome surfacing contract (what the prototype dropped)

`Returned` outcomes and terminal failures must reach PHP — never silently
dropped. Mechanism: a **pending error queue** on the `PublishBuffer`
(bounded, 4096 records; on overflow the oldest record is evicted and counted
in a `dropped_error_records` stat) holding
`{message_id, kind, message}` records:

| kind (record) | source | PHP exception class |
|---|---|---|
| `Returned` | mandatory return (unroutable) | `RabbitRsException` (as today) |
| `Nack`, `Timeout`, `Unconfirmed`, `InvalidRequest` | per-message terminal outcomes | `RabbitRsException` |
| `Transport` | connection-level failure | `ConnectionException` |
| `Backpressure` | actor intake refused | `BackpressureException` |
| `Closed` | pool/publisher closed | `RabbitRsException` (as `client_exception` maps today) |

Surfacing points — every PHP-visible operation drains the queue and throws
the first record's exception:

- `Pool::publish()` and `Pool::flush()` (sync-equivalent: today these throw
  at flush time; with the pipeline the same failure surfaces at the next
  operation),
- `Pool::size()`, `Pool::clear()`, `Pool::stats()` (flush/observability
  points),
- new `Pool::drainErrors(): array` — returns and clears the records
  (mirror of `Consumer::drainErrors()`),
- Laravel: `RabbitMqQueue::drainSettlementErrors()` (top of `pop()`)
  additionally drains `Pool::drainErrors()`: `Transport`/`Closed` →
  `ConnectionException::throw` (the Round I mechanism); every other kind →
  `QueueException`. A failed publish is therefore **observable at the next
  pop/flush/stats operation at the latest**, mirroring how settlement errors
  surface after pop.

Contract summary (at-least-once):

- Buffered-unconfirmed work survives connection recovery in bounded process
  memory and is replayed with the same `message_id` and original deadline —
  the existing actor invariant (tested mid-batch across a forced reconnect).
- Terminal outcomes (returned, timeout, nack, closed, transport, invalid
  request) are surfaced to PHP at the next operation — never silently
  dropped. Silent loss remains possible only where it is documented today:
  blind mode after hand-off, and process crash (in-memory replay is not
  durable; an external outbox is required for crash durability).
- Ordering: cross-batch publication order is best-effort (concurrent drains
  may interleave at the actor). Per-message at-least-once semantics and
  duplicate identification via `message_id` are unaffected.

## 4. PHP-caller-visible semantics (summary for release notes)

- `push()` returns before the broker confirms. A confirm failure is raised at
  the next `push`/`flush`/`pop`/`size`/`clear`/`stats` call at the latest, or
  can be polled explicitly via `Pool::drainErrors()`.
- Explicit `flush()` still blocks until every buffered publication is
  confirmed (or its failure surfaces), unchanged.
- Backpressure behavior, `onBackpressure`, `dropped_publications_total`, and
  all metrics keep their meanings; `drained`/pipelined failure modes add the
  `dropped_error_records` stat.
- No configuration surface: the pipelined flush is the behavior. The Phase 1
  experiment knobs (`RS_ASYNC_FLUSH`, `RS_FLUSH_MS`, `RS_FLUSH_MAX`,
  `RS_PROF`, `RS_TOKIO_WORKERS`) are removed — no kill switch (default
  answer per the task ruling); the sync flush paths remain as the escape
  hatch for callers who need synchronous confirmation (explicit `flush()`).

## 5. Test plan (TDD)

Rust core (paused time, mock transport): existing actor replay invariants
already pin same-message-id replay; add/verify a **mid-batch** variant
(batch published, connection lost while some confirms are outstanding,
recovery replays the unconfirmed with identical `message_id` and original
deadline).

PHP extension (Pest, `testingPool` mock scenarios):

1. auto-flush is non-blocking: threshold-triggered batch with pending
   confirmations must not stall the PHP thread for the confirm window;
2. `Returned` surfaces at the next operation (publish/flush/`drainErrors`),
   message and code intact, and is not re-buffered (no retry loop);
3. terminal failure (transport) surfaces via `drainErrors` and at the next
   pop/flush/stats at the latest;
4. re-buffered batches still retry (at-least-once) and dropped accounting
   works under teardown with outstanding drains;
5. quiesce: `flush()` after a spawned drain accounts every publication;
   destructor bounded.

Laravel integration: extend `RabbitMqQueue` drain path test — an unroutable
mandatory publish surfaces through `pop()` → `drainSettlementErrors()` as
`QueueException`/`ConnectionException` per the mapping above; chaos suite
(`AtLeastOnceChaosTest`) must stay green end to end.

Re-bench proof: fresh lab, same cells as the baseline
(`goopil-dispatch-blind`, `goopil-dispatch-safe`, `goopil-worker`,
`vladimir-dispatch` control), 3 passes × 10 rounds, release cdylib from this
worktree. Success: safe ≈ blind ceiling (~21 k), blind/worker/control
unchanged within session drift.
