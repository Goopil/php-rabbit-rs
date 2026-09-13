# Code Review — php-rabbit-rs (extension: FFI binding + core)

> **Scope**: `crates/rabbit-rs-php` (ext-php-rs 0.15.15 binding) + `crates/rabbit-rs-core` (lapin engine).
> **Branch reviewed**: `ref/sonarcloud-scope-and-fixes` — full adversarial review, ~30,688 LOC.
> **Status re-checked against v0.3.2** (2026-09-12): findings re-verified line-by-line at the `v0.3.2` tag; each HIGH carries its current status.
> **Method**: 3 passes (FFI/lifecycle, async core, tests/CI) + independent verification of every HIGH finding.
> Findings marked ✅ were verified line-by-line against the code.

**Verdict: 5 HIGH, 10 MEDIUM, 10 LOW.** The actor/pool architecture is solid (no deadlocks, no unbounded channels, publisher replay is correct), but 2 holes remain in the at-least-once contract and one crash channel to fix before production.
**v0.3.2 status**: no product fix in v0.3.2 (tests/benches/deps + the laravel package's auto-scaling supervisor #262). `crates/rabbit-rs-core/src` is untouched; the only binding diff is visibility (`pub(crate)` → `pub`) plus a `#[doc(hidden)] bench_api` module for the CodSpeed benches — behavior unchanged. HIGH 2 is half-fixed (#233); HIGH 1, 3, 4, 5 still open (re-verified at the tag).

---

## 🏗️ Architecture

- **The at-least-once contract has two holes on the PHP side** (HIGH 1 + 2 below): the semantics are impeccable in the core, but PHP destruction paths (Pool GC, consumer teardown) reintroduce silent loss the core forbids itself.
- **Self-healing asymmetry: consumer vs publisher** — the consumer replaces its source on error; the publisher has no equivalent for a channel-level error (HIGH 4).
- **Teardown budget asymmetry**: publish = 500 ms total (audit F-18), consumer = 2 s **per channel, no overall budget** (HIGH 2). An N-broker profile can freeze an FPM/Octane worker for N×2 s at shutdown.

---

## 🔴 HIGH

### 1. ✅ STILL OPEN @v0.3.2 — `tearing_down` on a PublishBuffer shared between Pool and Consumer → silent permanent drop of accepted publications
`crates/rabbit-rs-php/src/classes/pool.rs:422-441` + `publish_buffer.rs:301-310, 511-513` (@v0.3.1: `publish_buffer.rs:312, 617`)

`Pool::consumer()` shares the **same** `Arc<PublishBuffer>` with the Consumer, then `Pool::__destruct` → `flush_teardown()` → `tearing_down.store(true)`. `rebuffer_or_drop()` then discards any publication whose pipelined drain fails:

```rust
if self.tearing_down.load(Ordering::Acquire) || self.client.is_closed() {
    self.dropped_publications.fetch_add(...);
    return; // re-buffer refused
}
```

The "nobody will flush them again" comment is **false** in this pattern: the still-living Consumer (and the client, kept alive by `ConnectionHandle::client`) could flush on the next `next()`. In Octane usage (per-request publish pool + long-lived consumer from the same pool): a broker flap at a request boundary → accepted publications (message_ids already returned to the caller) counted in `dropped_publications_total` and destroyed, **without any PHP exception**. Direct violation of AGENTS.md's "silent loss is unacceptable". Fix: don't flag `tearing_down` while a consumer still holds the buffer, or separate publish/consumer buffers.

### 2. ✅ HALF-FIXED IN 0.3.0 (#233) — consumer close now drains settlements within a 500 ms budget; the per-channel close stall remains
`crates/rabbit-rs-php/src/classes/consumer.rs:238-257` + core `consumer/set.rs:572-583`

**Fixed half (0.3.0, #233):** acks no longer race the close — `CLOSE_SETTLEMENT_DRAIN_BUDGET = 500 ms` (`consumer/actor.rs:1022` @v0.3.1) sweeps in-flight and queued settlements to the transport before channels close, mirroring the publish side's F-18 budget. The silent-ack-loss half of this finding is resolved.

**Still open @v0.3.1:** `ConsumerSetHandle::close()` awaits a oneshot **with no timeout**, and the actor still closes each channel with a 2 s budget **sequentially** (`consumer/actor.rs:1112-1115`):

```rust
for runtime in state.subscriptions.values() {
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), runtime.channel.close()).await;
}
```

An N-broker profile against N stalled brokers still blocks the FPM/Octane worker for up to N×2 s inside `__destruct` at request shutdown — the stall half of F-18's mirror remains. Fix: an overall teardown deadline across the channel loop.

### 3. ✅ STILL OPEN @v0.3.2 — any Rust panic kills the worker, and the one realistic source is an `eprintln!`
`crates/rabbit-rs-php/src/sink.rs:37` (still present at the tag) (+ ext-php-rs 0.15.15 `zend/try_catch.rs:92-98`)

ext-php-rs catches the panic then **resumes it** through the `extern "C"` handler → abort → the FPM worker dies with all its unconfirmed publications. The binding sweep is clean (no reachable unwrap/`as`/indexing), BUT:

```rust
eprintln!("[rabbit-rs.{}] {}: {}", ...);
```

`eprintln!` **panics** if stderr is closed/rotated (daemonized FPM, dead log collector, EPIPE). The core's `Sink` contract demands "must not panic" (`rabbit-rs-core/src/log.rs:22-23`) — the binding's sink violates it, and it runs on tokio worker threads where ext-php-rs's catch machinery doesn't apply: the panic kills the connection's actor task. With `RABBIT_RS_LOG=warn` in production, a broken stderr = dead connection. Fix: `write!` with the result ignored, or non-panicking `std::io::stderr().write_all`.

### 4. ✅ STILL OPEN @v0.3.2 — the publisher never self-heals from a channel-level error on a healthy connection
`crates/rabbit-rs-core/src/publisher/actor.rs:777-784` (terminal branch at `:788` @v0.3.1) + `transport/lapin.rs:96-101`

A failed publish has only two outcomes: replay+suspend (recoverable) or terminal failure — **neither replaces the channel**. Yet a broker-side **channel** close (404 on a mandatory publish to an unknown exchange, 406 message > max_message_size) kills the lapin channel while the connection stays healthy, and the error stream only forwards **connection** events:

- terminal branch: the actor stays `Ready` with a dead channel → every subsequent publish fails;
- recoverable branch: suspends indefinitely → buffer (≤1024) then `Backpressure` **forever**.

Publishing to a mistyped exchange requires a connection outage to recover. The consumer self-heals (`SourceReplaced`); the publisher deserves the same `ChannelReplaced`.

### 5. ✅ STILL OPEN @v0.3.2 — a single message larger than `max_buffered_bytes` permanently wedges a consumer set
`crates/rabbit-rs-core/src/consumer/actor.rs:544-546, 594-596` (@v0.3.1: `drain_pending()` break at `:536-538`)

An over-budget delivery parks at the head of `pending_incoming` and blocks the FIFO:

```rust
let over_budget = ... current.saturating_add(delivery_bytes) > *max ...;
if over_budget { break; }
```

(0.3.x change: `pending_incoming` is now count-bounded — audit F-04, `no_ack` growth protection — but the over-budget single-message wedge itself is untouched.)

When `pending_incoming` reaches `pending_capacity`, the `select!` command arm is disabled → pumps block → the embedder waits inside `next()` on an empty buffer. RabbitMQ tolerates 16–128 MB messages > the 64 MiB default → **permanent silent wedge**, which repeats on every redelivery after recovery. Test `tests/consumer.rs:1017-1059` only covers small payloads. Fix: isolate the over-budget message (deliver it alone + warn, or nack/DLX), and reject `max_buffered_bytes: 0` at validation (see LOW 15).

---

## 🟡 MEDIUM

### Binding (crates/rabbit-rs-php)

6. **`publish_buffer.rs:511-533`** — At the end of every FPM request (Pool GC without an explicit `flush()`), any pipelined drain unconfirmed within **500 ms** is counted as dropped. 500 ms < broker RTT tail under load, while the same code grants a 30 s deadline to an explicit `flush()`. (0.2.2's #218 added the background `flush_interval` timer, which removes the *invisible lone batch* case — the GC-time 500 ms teardown budget itself is unchanged.) In steady-state FPM this is the main loss path — document it in an ops runbook keyed on `dropped_publications_total`, or align it with the explicit-flush deadline.
7. **`pool.rs:132-145` + `publish_buffer.rs:341-353`** — The soft buffer ceiling turns `BackpressureException` (contract: fail fast, "retry later") into a **full-deadline** synchronous flush: up to 30 s stall per call at the 4097th buffered message during an outage. Same pattern on `Consumer::next` → `drain_publish_buffer` → `flush_all` before the `try_next` fast path. Latency cliff users will perceive as "the worker hangs during the RabbitMQ outage".
8. **`pool.rs:505-515` + `publish_buffer.rs:177-187`** — Auto-surface drains up to 4096 error records and raises **only the first**; the rest (including `Returned`/unroutable records with message_ids) are destroyed with no counter (`dropped_error_records_total` only counts evictions, not surface-time discards). The operator knows *that* something failed, never *what*. Only `drainErrors()` preserves everything — nothing steers users toward it before the next publish/flush/size/clear/stats silently eats the queue.
9. **`bridge.rs:104-108` + `pool.rs:156-158`** — Callback exceptions overwritten or swallowed: `let _ = exception.throw()` contradicts its own comment ("cannot fail" — throwing while a PHP exception is already in flight is a real path), and `drain()` before `surface_publish_errors()` lets the publish error overwrite the callback's exception. F-17 fixed destruction, not overwrite.

### Core (crates/rabbit-rs-core)

10. **`pool/connection_actor.rs:437-445`** — Backoff resets to `failures: 1` **on every loss**: a broker that accepts then resets in a loop (LB drain, OOM kill, credential churn) is hammered at ~100 ms **forever**, never escalating to `max_interval` 30 s. One pool actor per PHP worker × a fleet = self-inflicted DDoS on a dying broker.
11. **`client.rs:715-735`** — `ClientPool::publisher()`: the `wait_for_state` predicate matches **every** variant of `ConnectionState` → returns immediately → the loop degenerates into a hot poll of the publisher mutex + watch for the entire recovery (including replay under mutex). Runs on the PHP thread via `block_on`: burned CPU + lock contention on every acquisition during an outage.
12. **`client.rs:386-391` + `pool/recovery_coordinator.rs:331-339`** — A consumer establishment failure (404 on a queue name, access-refused) is swallowed by `.ok()` then reported as a **connection loss** → the coordinator destroys the whole generation: every healthy consumer is torn down/re-established, the publisher suspends and replays — and the bad profile fails again on the fresh generation. One typo'd queue = rolling outage of the entire connection, root cause hidden behind a timeout error.
13. **`consumer/actor.rs:1305-1320` + `actor.rs:469-475` + `set.rs:29-32`** — Poison without a DLX = **ack + destroy** after 20 attempts, with the only trace being a `MaxAttempts` error routed through a bounded drop-oldest channel (256, shared by the whole set). A poison storm (topic fanout) destroys every copy while the embedder, busy with the first wave, loses most notifications. Two individually-documented behaviors whose intersection is a silent-loss amplifier — deserves a louder contract (a dedicated metric at minimum).
14. **`docs/plans/2026-07-30-rabbitmq-native-design.md:109` vs `publisher/actor.rs:498-502`** — The design doc says "the deadline is never reset by a reconnection"; the code re-arms **once** with a fresh deadline (and AGENTS.md:87 agrees with the code). Stale doc: anyone sizing SLAs from the design doc gets the wrong model. Delete or fix the doc.

---

## 🔵 LOW

15. **`config.rs` validate() (~565-658)** — `max_buffered_bytes: 0` accepted (cheapest trigger for HIGH 5), `delivery_limit: Some(0)` accepted (no-op quorum value), `port: 0` accepted. Missing validation at the trust boundary.
16. **`scripts/install.sh:14-15`** — The comment claims "release by default" but bare `exec cargo php install` installs a **debug** cdylib system-wide: overflow checks panic (combos with HIGH 3 → abort) and ~10× slower.
17. **`pool.rs:197-199, 422-425`** — `publishBatch` calls `flush()` first: a closed/fork-inherited pool throws "flush cannot use a closed pool" from a `publishBatch` call (wrong attribution). And `__destruct`'s PID guard returns **before** `flush_teardown`: a forked child abandons inherited buffered publications with no counter (at-least-once holds via the parent, but the child's `stats()` lie until `acquire()`).
18. **`core/runtime.rs:229-238`** — `mem::forget(runtime)` after fork: correct about threads, but it also skips dropping transports → the child keeps the parent's AMQP socket FDs open until exec/exit. Dead socket accumulation in repeatedly-forking daemons. Documented tradeoff, unquantified.
19. **`stubs/rabbit_rs.stub.php:44, 156, 232`** — The stubs declare public `__construct()` on `Consumer`, `Delivery` and both exceptions while the runtime forbids construction (`ReflectionTest.php:53-56`). Static analyzers bless code that crashes.
20. **`consumer.rs:93-129` + core `set.rs:493`** — `nextBatch(0, timeout)`: 0 passes the binding, the core clamps to 1 → asking for zero deliveries parks until the timeout and returns one. Coherent with the stub ("clamped to 1..=256") but surprising as an API.
21. **`benchmarks/README.md:172`** — `cargo bench -p rabbit-rs-core` while the crate has neither `benches/` nor criterion → the command runs the test suite. The benchmark methodology itself is honest (broker RTT included, warmup, interleaved A/B, 0-loss gate).
22. **`consumer/actor.rs:926-928`** — `affected_tokens.last().unwrap()`: safe today (the `validate_contiguous_prefix` invariant) but an invariant-dependent `unwrap` in library code; name the invariant or use `unwrap_or_default`.
23. **`transport/lapin.rs:540-543`** — TTLs silently truncated at `u32::MAX` ms (~49.7 days): a 60-day `x-message-ttl` becomes 49.7 days without a warning.
24. **`consumer/set.rs:378-387` + `recovery_coordinator.rs:304-308`** — `ConsumerSetHandle` close-on-drop is clone-unsafe: dropping one clone closes the set the coordinator's map still holds, which may then serve the closed handle until the next generation bump. The comment admits it; the type doesn't prevent it.

---

## ✅ Verified clean

- **No deadlocks**: no `StdMutex`/`RwLock` guard held across an `.await` (RequestedProfiles, EstablishLock, pump RwLock, close_completion).
- **No unbounded queues**: all channels bounded; publisher semaphore + byte budget; `pending_incoming` count-bounded.
- **Acks across generations**: generation-tagged settlements, stale ones rejected, contiguous-prefix validation — no double-decrement of byte budgets.
- **Safe-mode publisher replay**: ledger → confirm wait → replay → suspend/flush → single re-arm; `tests/recovery.rs` covers replay after recovery.
- **Runtime**: one process-global multi-thread runtime (1 worker, measured tradeoff), 2 s shutdown, fork-safe via PID check.
- **TLS**: `verify: none` rejected at config and transport.
- **Real binding tests**: Pest runs with `php -d extension=<fresh cdylib>` + 5 PHPT; the `testing_pool` mock is feature-gated out of release builds. Honest suite.
- **Topology reconciler**: idempotent per (generation, plan), delayed-exchange/queue delete hygiene.

---

## Recommended priorities

1. **HIGH 1** (silent loss via shared-buffer `tearing_down`) — the only remaining at-least-once hole on the PHP side.
2. **HIGH 2 (stall half)**: overall teardown deadline across the per-channel close loop (the ack-loss half is fixed by #233).
3. **HIGH 3**: replace the `eprintln!` (5 lines, removes the only realistic crash channel).
4. **HIGH 4 + 5** (liveness): `ChannelReplaced` on the publisher side + isolation of over-budget messages.
5. **MEDIUM 10-12** (recovery robustness under flap): backoff accumulation, `wait_for_state` predicate, establishment-error classification.
6. LOWs as you go; start with 15 (validation) and 16 (install.sh), one line each.
