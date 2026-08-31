# Red-team technical audit — Rabbit RS (2026-08-31)

**Auditor:** euria-code (independent external audit)
**Scope:** full workspace — `crates/rabbit-rs-core`, `crates/rabbit-rs-php`, `packages/laravel-queue`, `benchmarks/`, `scripts/`, `.github/`, `lab/`, `docs/`
**Method:** evidence-first adversarial audit. Every finding was confirmed by direct code reading; where stated, dynamic verification was run on this machine (macOS ARM64, PHP 8.5.6, Rust 1.96, RabbitMQ 4.x 3-node lab). Confidence levels: **Confirmed** (demonstrable from the repository or reproduced), **Likely** (very probable, one external datum missing), **Needs verification** (credible, inconclusive).
**Prior audits reviewed as baseline:** `docs/audit/2026-07-31-strict-audit.md`, `docs/audits/2026-08-01-milestone-b-audit.md`.

---

## 1. Executive summary

**Overall score: 6/10 — strong engineering, not yet production-certified against the contract it promises.**

Rabbit RS is an unusually disciplined codebase: `#![forbid(unsafe_code)]`, lapin hidden behind a mockable transport, bounded queues everywhere, paused-time deterministic tests, a release pipeline with real attestations. The two previous audits were acted upon: all 5 HIGH findings of July 31 and the 1 critical + 2 high of August 1 are verifiably fixed (§3).

However, this re-audit found one **contract-level correctness issue the project has itself observed and worked around in its benchmark** (deliveries silently missed when a consumer is idle during a publish burst), two **High** correctness bugs (poison-message infinite loop enabled by dead max-attempts enforcement; delayed jobs silently delivered immediately in TTL mode), and a cluster of **silent-failure** issues (no logging facility in the core, swallowed teardown errors, debug-only boundary validation). Observability is the weakest domain: at 3 a.m., a Rabbit RS outage is invisible from the inside.

| Domain | Score | Domain | Score |
|---|---|---|---|
| Correctness | 6/10 | Data integrity | 5/10 |
| Architecture | 8/10 | Testing | 7/10 |
| Performance | 6/10 | Maintainability | 7/10 |
| Scalability | 7/10 | Observability | 4/10 |
| Stability | 6/10 | Operations | 6/10 |
| Resilience | 7/10 | Developer experience | 7/10 |
| Security | 7/10 | **Overall** | **6/10** |

**Risk level: HIGH** for the at-least-once no-silent-loss contract; MEDIUM for everything else.

### Top 10 problems

1. **AUDIT-001** — Consumer left idle during a publish burst misses ~2% of deliveries (verified by the project's own benchmark; a reconnect workaround is in place in the bench). Contract breaker.
2. **AUDIT-002** — Max-attempts enforcement is dead code; attempts collapse to 1–2; Laravel `--tries` never triggers; poison messages redeliver forever by default.
3. **AUDIT-003** — TTL delay routing failure silently publishes the message **immediately** to its original destination.
4. **AUDIT-004** — `delay.mode = auto` (the default) never detects the plugin and never falls back to TTL, contrary to the design.
5. **AUDIT-005** — Consumer-set `Drop` close is best-effort and losable; leaked actor + AMQP channels live forever in long-lived processes.
6. **AUDIT-013** — Zero logging in the core; recovery failures are `eprintln!` (discarded under FPM); dead actors leave stale `Ready` state on the watch.
7. **AUDIT-011** — Release pipeline: Packagist token in URL without `--fail` (silent release failures + token in logs on error).
8. **AUDIT-012** — Release binaries built from floating `php:X-cli/alpine` Docker images; composer installer fetched without checksum; no `composer.lock` anywhere.
9. **AUDIT-007** — A failed on-demand consumer establishment tears down the whole broker connection (publisher replay + all consumers).
10. **AUDIT-020** — The chaos/at-least-once test suite depends on a Toxiproxy setup the lab no longer provisions — the most important tests silently skip.

### Immediate recommendations

1. Reproduce and fix AUDIT-001 (idle-consumer stall) — it invalidates the core promise.
2. Enforce `MaxAttempts` at dispatch (AUDIT-002) — small diff, removes the infinite-loop enabler.
3. Make delay-routing failures fail the publish instead of degrading (AUDIT-003) and implement or remove the `auto` TTL fallback (AUDIT-004).
4. Introduce `tracing` in the core and broadcast `Closed` on actor exits (AUDIT-013).
5. Repair the release secret handling and pin the release base images (AUDIT-011/012).

---

## 2. System model (Phase 0)

Three layers, one process = one registry:

- **`crates/rabbit-rs-core`** (~20k LOC): per-broker `RecoveryCoordinator` owning a `ConnectionActor` (states Disconnected→Connecting→Ready→Recovering→FailedPermanent, exponential backoff 100 ms→30 s, equal jitter, generation increments on successful connect). A `PublisherActor` per broker: bounded command queue (1024), global byte budget (64 MiB), replay buffer merging replay+publishing+ledger on suspend, publisher confirms with per-waiter deadlines, mandatory-return precedence, blind-mode pump with its own bounded intake. `ConsumerSet` per broker: per-subscription byte-bounded buffers, deficit-weighted round-robin scheduler with priority aging, per-channel settlement ledgers with contiguous-prefix `ack(multiple)`, delivery tokens carrying `(connection_key, generation, channel_id, delivery_tag)`. Delay routing: plugin (`<exchange>.delayed` + `x-delay`) or TTL buckets (SHA-256-named durable quorum queues, DLX back to destination).
- **`crates/rabbit-rs-php`**: ext-php-rs extension (`Pool`, `Consumer`, `Delivery`). All blocking calls `block_on` a process-global Tokio runtime (1 worker thread, lazy, 2 s shutdown budget). Fork detection at two levels (per-object PID guard; registry `mem::forget` of inherited runtime). PHP callbacks stored as `Zval`, invoked only on the PHP thread inside `EventBridge::drain()`.
- **`packages/laravel-queue`**: standard Laravel driver (queue:work loop preserved), Horizon adapter, Octane lifecycle hooks, `rabbit-rs:work` supervisor (fork or Symfony Process children), aggressive `ConfigNormalizer` (path-addressed errors, delivery_limit-requires-dead_letter rule, ack-flag chain rules).

Publishing is at-least-once, process-local: unconfirmed publications survive connection loss in bounded memory and are replayed with the same `message_id` and original deadline; a PHP crash loses them (documented; external outbox out of V1 scope). Settlements are fire-and-forget with errors surfaced asynchronously via `drain_errors()`.

**Dynamic verification performed during this audit** (macOS ARM64, PHP 8.5.6, Rust 1.96, RabbitMQ 3-node lab on Docker/Colima):

- `cargo nextest -p rabbit-rs-core`: **275 tests passed**.
- Laravel Pest Unit/Feature (no extension): **247 tests, 620 assertions passed** (8 PHPUnit notices).
- `cargo nextest -p rabbit-rs-core --features integration --test integration`: **23 tests passed** against the live lab.
- Laravel Pest Integration (extension loaded): push/pop/delete, bulk, raw payload, release requeue, size-after-clear, bad-credentials — **PASS**; the chaos scenarios asserted their internal at-least-once property (`missing = 0`) in every executed scenario, but see AUDIT-020: on this machine the toxics were injected into an **unrelated Toxiproxy** listening on the same port, so those internal PASS lines are vacuous.
- `it publishes and consumes after delay` **fails** on a plugin-less lab — exactly the failure mode predicted by AUDIT-004 (mode `auto` always routes plugin; no TTL fallback).
- `it increases size after push` **fails reproducibly** (0 after 2 confirmed pushes) → AUDIT-031.
- Horizon unit tests are not extension-agnostic: with the real extension loaded they fail on fake-class signatures — consistent with the documented Unit/Feature-without-extension gating, worth a harness guard (P3).
- Previous-audit regression checks: compiled Rust suites + targeted code reads (§3).

---

## 3. Baseline — status of the two previous audits

Every previously reported defect was re-checked. **None regressed.**

| Previous finding | Status | Evidence |
|---|---|---|
| 07-31 #1 — `message_id` lost at consumption | **FIXED** | `transport/lapin.rs:293-300` extracts message/correlation ids; dispatch reuses them (`consumer/actor.rs:219-226`) |
| 07-31 #2 — publisher generation `<=` bug | **FIXED** | `publisher/actor.rs:630` rejects only `generation < state.generation`; `Recovering{N}`→`Ready{N}` accepted |
| 07-31 #3 — corrupted consumer tag | **FIXED** | `consumer/set.rs:187` uses `subscription.id.as_str()` |
| 07-31 #4 — unbounded `source_errors` | **FIXED** | `consumer/actor.rs:31` `SOURCE_ERROR_CAPACITY = 64`, drop-oldest |
| 07-31 #5 — runtime drop can hang | **FIXED** | `runtime.rs:202-225` shared 2 s budget + `shutdown_timeout`; fork path uses `mem::forget` (correct for forked threads) |
| 07-31 #6 — hardcoded 30 s delayed-release deadline | **FIXED** | `consumer/actor.rs:888` uses `publisher.confirm_timeout()` |
| 07-31 #7 — delivery absorbed by expired waiter | **FIXED (architecture)** | pull-based flume buffer; over-budget deliveries pushed back, never dropped (`consumer/actor.rs:307-314`, `set.rs:362-385`) |
| 07-31 #8 — `spawn` without rollback | **FIXED** | `consumer/set.rs:177,195` closes opened channels on failure |
| 07-31 #9 — credential-bearing URI to lapin | **MITIGATED, OPEN** | URI still carries credentials (`lapin.rs:373-376`) — see AUDIT-027 |
| 07-31 #11 — missing `Reject` settlement | **FIXED** | `Settlement::Reject` (`consumer/delivery.rs`), executed at `actor.rs:834-840` |
| 08-01 #1 (critical) — expired waiter absorbs delivery | **FIXED (architecture)** | waiters removed; buffer pull model + timeout test (`consumer.rs` consumer wait deadline) |
| 08-01 #2 — close/create race | **FIXED** | generational commit pattern (`client.rs:403,660,696,774`); loser closes its resource |
| 08-01 #3 — unbounded PHP-boundary memory | **FIXED** | 256 msgs / 1 MiB / 128 headers / 64 KiB budgets (`crates/rabbit-rs-php/src/conversion.rs:14-18`, `publish_buffer.rs:29-31`) |
| 08-01 #4 — extreme durations / unbounded shutdown | **FIXED** | `MAX_TIMEOUT_MS` + `checked_add` (`conversion.rs:128-139`); 2 s shutdown budget |

The fix backlog was executed thoroughly. The findings below are **new**.

---

## 4. Findings

Format: severity / confidence / domain / location. "Confirmed" = demonstrated from the repository; repro notes included where cheap.

---

### AUDIT-001 — Consumer left idle during a publish burst silently misses deliveries (~2%)

- **Severity:** Critical
- **Confidence:** Confirmed (documented and observed by the project itself; root cause **needs verification**)
- **Domain:** Data integrity / Correctness
- **Location:** `benchmarks/driver-bench/bin/bench.php:134-141`, `:171-192`, `:416-421`; consumer pipeline `crates/rabbit-rs-core/src/consumer/{set,actor,composite}.rs`

### Problem

The project's own benchmark documents, in comments, that an `ext-rabbit_rs` consumer which exists **before** a publish fill and stays idle while the fill is ingested never surfaces ~2% of the messages ("verified: consumer created pre-fill → ~2% of messages never surface; consumer created after the fill → clean"), and that under pop+ack churn "the consumer can stop receiving deliveries while messages remain ready in the queue". The bench works around both by rebuilding the connection per round and reconnecting mid-drain.

### Evidence

- `bench.php:134-141` — warmup section, quoted above.
- `bench.php:171-192` — "Fresh connection per round: a consumer left over from the previous round and left idle while the next fill is ingested misses deliveries"; and "the tail of the fill is still in flight when the first pops run and a few messages surface late (observed on the ext-rabbit_rs consumer)".
- `bench.php:416-421` — drain-side reconnect workaround.
- The Rust integration suite creates consumers **after** publishes, so the deterministic suites never exercise idle-consumer-during-fill; 275 Rust tests + integration tests pass, which is consistent with the blind spot.

### Impact

A silent loss path in a library whose founding invariant is "no silent loss in at-least-once scenarios". Production relevance: Laravel workers and Octane apps keep consumers alive across idle gaps (the consumer is created at the first `pop()` and reused), which is precisely the pre-fill-idle pattern. If reproducible in production topology, jobs are lost with no error, no metric, no log.

### Trigger

Consumer exists (subscription registered, prefetch granted) → publisher bursts N messages → consumer starts/continues popping. ~2% never surface; occasionally the consumer stops receiving entirely until reconnect.

### Verification

1. Add a deterministic mock-transport test: create `ConsumerSet`, deliver nothing, then inject a burst of deliveries > prefetch, then pop in a loop; assert all deliveries surface.
2. Against the lab: publish 10k messages, create the consumer first, drain with pop+ack, count.
3. Instrument: count `Incoming` commands accepted by the actor vs deliveries dispatched to the flume vs deliveries surfaced to PHP — find where the 2% vanishes.

### Recommended fix

Treat as a P0 correctness investigation, not a benchmark curiosity. Add the repro as a non-regression test first (test-driven), then fix the actual hop (candidate suspects: per-source pump exit on transient `send` error, `dispatch_notify` lost-wakeup between `try_next`'s `Ok(None)` path and a full flume, or lapin stream drop on channel reuse).

### Regression risk

Low for the fix itself; the workaround in the bench must be removed once fixed, otherwise the bench keeps masking regressions.

---

### AUDIT-002 — Max-attempts enforcement is dead code; attempts collapse to 1–2; Laravel `--tries` never triggers; poison messages loop forever by default

- **Severity:** High
- **Confidence:** Confirmed
- **Domain:** Correctness / Data integrity
- **Location:** `crates/rabbit-rs-core/src/consumer/actor.rs:228-230`; `crates/rabbit-rs-core/src/consumer/attempts.rs:66-79`; `packages/laravel-queue/config/rabbit-rs.php:387-389`

### Problem

```rust
let attempts = AttemptsResolver::default()
    .resolve(&delivery.headers, delivery.redelivered)
    .unwrap_or(if delivery.redelivered { 2 } else { 1 });
```

`resolve()` returns `Err(MaxAttempts)` exactly when attempts > 20 (`attempts.rs:66-79`) — the one case the cap exists for. The `unwrap_or` swallows it: a message at attempt 25 is delivered with `attempts = 2`. Consequences chain:

1. `RabbitMqJob::attempts()` reports 1–2 forever → Laravel `Worker::markJobAsFailedIfAlreadyExceedsMaxAttempts` never fires → `--tries=N` is neutralized.
2. `ConsumerErrorKind::MaxAttempts` (mapped to a PHP exception in `exception.rs`) is unreachable from the dispatch path.
3. Default Laravel config sets `delivery_limit => null` and `dead_letter => null` (`config/rabbit-rs.php:387-389`), so no `x-delivery-limit` is declared either.
4. Net result: a poison message (job that throws) redelivers **forever**, grinding the worker pool, with no application- or broker-side stop. The config file itself documents the crash-redelivery variant of this risk (`config/rabbit-rs.php:399-401`) and mitigates it with a one-per-process production warning (`RabbitMqConnector.php:97-127`) — but a warning does not stop the loop, and the attempts bug makes even `--tries` useless.

### Evidence

Code paths above; `attempts.rs` is the only resolver and its `Err` is only produced for `MaxAttempts`; `delayed_headers` (the other `validate()` caller, `actor.rs:878-880`) receives the already-collapsed value, so the cap can never trigger there either.

### Impact

Infinite redelivery loop; worker pool exhaustion by one bad job; misleading telemetry (attempts metrics lie); Laravel failed-jobs machinery never engages for this driver.

### Trigger

Any job that repeatedly throws, on a queue without `x-delivery-limit`, after 20 deliveries (quorum queues provide `x-delivery-count`; classic queues reach the cap via the app header only on delayed release — the collapsed value resets that too).

### Verification

Unit test: deliver a message with `x-delivery-count = 25`; assert the delivery reports attempts = 2 (bug) instead of failing with `MaxAttempts` or reporting 25.

### Recommended fix

At dispatch, treat `MaxAttempts` as a terminal condition: reject without requeue (or reject with requeue only when a DLX is configured) and surface a typed error; otherwise at minimum return the true resolved attempts (remove the cap-swallowing) so Laravel `--tries` works. Keep `max_attempts` configurable via the native config.

### Alternatives

Rely solely on broker `x-delivery-limit` + mandatory DLQ config (reliable but forces DLQ topology on every user and changes default semantics).

### Regression risk

Low. Tests exist for the resolver (`tests/topology.rs:927-945`); add the dispatch-path test.

---

### AUDIT-003 — TTL-mode delayed publish falls back to **immediate delivery** on routing error

- **Severity:** High
- **Confidence:** Confirmed
- **Domain:** Correctness
- **Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:788-812` (and `:967-973`); `crates/rabbit-rs-core/src/topology/delay.rs:62-68`

### Problem

```rust
.and_then(|strategy| DelayRouter::route(...).ok()...)   // error discarded
...
let (exchange, routing_key, delay_ms) = routed.unwrap_or((
    request.destination.exchange.clone(),   // original destination
    request.destination.routing_key.clone(),
    request.properties.delay_ms,
));
```

When `DelayRouter::route` fails — e.g. TTL mode with `delay > largest bucket` ("delay exceeds the largest configured TTL bucket", `delay.rs:67`) — the `.ok()` discards the error and the message is published to the **original exchange with no delay**. On a normal exchange the `x-delay` header is inert: a job scheduled for later executes **immediately**. Default Laravel buckets are `[1, 5, 30, 120]` seconds (`ConfigNormalizer.php:493`), so `later(300)` in `mode: ttl` silently runs now. The consumer-side delayed release handles the same error correctly (`consumer/actor.rs:874-875` returns the error) — the publisher path is the inconsistent one. `ensure_delay_topology` repeats the same `.ok()`-and-proceed pattern (`publisher/actor.rs:967-973`).

### Impact

Incorrect business timing (notifications, retries, billing reminders fire hours early), invisible: no error, no metric, confirmed as a normal publish. Compare: plugin mode cannot hit this (route always succeeds), so default installs are safe until someone selects `ttl` (the documented fallback when the plugin is not allowed).

### Verification

Test: `mode = ttl`, buckets `[1,5,30,120]`, publish with `delay_ms = 300` on the mock; assert the publish goes to the original exchange (bug) instead of failing.

### Recommended fix

Propagate the routing error as a `PublishError` (like the consumer path does); validate the maximum supported delay at config load and surface it in `ConfigNormalizer` so users learn the ceiling before deploying.

---

### AUDIT-004 — `delay.mode = auto` never detects the plugin and never falls back to TTL

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Correctness / design divergence
- **Location:** `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:630-642`; `crates/rabbit-rs-core/src/config.rs:236-243`

### Problem

The design (and Task 29 of the implementation plan) promise: auto mode detects `rabbitmq_delayed_message_exchange` and falls back to TTL buckets. Reality: `compile_delay_strategy` maps `DelayMode::Auto` to `DelayStrategy::Plugin` **unconditionally** (`DelayMode::Plugin | DelayMode::Auto => DelayStrategy::Plugin`), no detection exists anywhere (`rg DelayMode` — single consumer), and `DelayConfig` has no `detection_timeout` field (the plan's Task 29 mentions validating one; it was dropped). Consequence for a user relying on the documented default without the plugin: every `later()` publishes to `<exchange>.delayed`, whose lazy `x-delayed-message` declaration fails → publish error (loud, at least) — but the promised graceful fallback does not exist, and the TTL bucket code path is effectively dead unless explicitly selected.

### Recommended fix

Either implement detection (declare-or-probe once per generation, bounded by a timeout, cached like `declared_ttl_queues`) or change the design/docs to say `auto` = `plugin`, and document the failure mode without the plugin. The current state is the worst of both: the config value is a lie.

---

### AUDIT-005 — Consumer-set close-on-drop is best-effort and losable → leaked actor + AMQP channels forever

- **Severity:** Medium
- **Confidence:** Confirmed (code); trigger requires channel-full at drop time
- **Domain:** Resilience
- **Location:** `crates/rabbit-rs-core/src/consumer/set.rs:298-306`, `set.rs:216-227`, `consumer/actor.rs:555`

### Problem

`ConsumerSetHandle::Drop` does `try_send(Close)` — best-effort. The actor holds its **own** clone of the commands sender (`set.rs:219`), so the `None => return` arm (`actor.rs:555`, "all senders dropped") is unreachable: the actor can only stop via a `Close` command. If the command channel (`max(256, total_prefetch)`) is momentarily full when the handle drops, the `Close` is silently lost and the actor task plus its open consumer channels live for the lifetime of the pool — in Octane, potentially for days — while the broker keeps pushing deliveries into a buffer nobody reads (they remain unacked until connection close). Every guarded close path (`recovery_coordinator.rs:591-594`, `client.rs:406`, composite `close()`) uses `close().await` and is safe; the Drop path is the last-resort net (PHP `Consumer::__destruct` with a fork-guard throw, block_on failure, etc.) and it has a hole.

### Recommended fix

Make the actor exit when its *own* retained sender is dropped (hold the sender in an `Option` and drop it at task start — the classic "make drop of all external handles mean channel closure" pattern), or spawn a dedicated watchdog task that holds only the receiver and terminates the actor on dropout. Also count lost `Close` commands as a metric.

---

### AUDIT-006 — Topology plan compile failure silently downgraded to External

- **Severity:** Medium (rare trigger)
- **Confidence:** Confirmed (code); trigger mostly via direct Rust/extension configs
- **Domain:** Correctness / observability
- **Location:** `crates/rabbit-rs-core/src/client.rs:743-749`

### Problem

`TopologyPlan::compile(...).unwrap_or_else(|_error| TopologyPlan::compile(External, empty)...)` — an invalid topology definition is swallowed at pool start; the pool then declares nothing and publishes/consumes against whatever the broker already has. No log, no metric, no error. In `declare` mode this silently violates the operator's declared intent (e.g. a DLQ they expect to exist never gets created — and AUDIT-002's poisoned messages then drop or loop).

### Recommended fix

Fail pool creation (config error with the compile message) or at minimum record a source error + metric. Silent mode-switching at a topology boundary is the exact class of bug the July audit's "configuration failures must identify their exact input path" rule exists to prevent.

---

### AUDIT-007 — Failed on-demand consumer establishment tears down the whole broker connection

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Resilience
- **Location:** `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:274-283`

### Problem

`RecoveryCoordinatorHandle::consumer()` runs `establish_requested_profile`; on any error it sends `connection_lost(...)` — a **recoverable** connection-level event. One failed channel open / QoS call for one profile drops the shared connection, suspends the publisher (replay cycle), and closes/re-establishes every other consumer on that broker. Blast radius of a local error is global to the broker. The same coupling exists in the recovery loop rollback (`:366-377`), which is legitimate there (recovery must retry) but not for the on-demand path.

### Recommended fix

Retry the establishment with the recovery policy scoped to that profile; only escalate to `connection_lost` after N profile-level failures.

---

### AUDIT-008 — Publish-message unknown-key validation is debug-build-only

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Correctness / security boundary
- **Location:** `crates/rabbit-rs-php/src/conversion.rs:91,168` (`let validate_keys = cfg!(debug_assertions);`)

### Problem

Unknown keys in the publish message hash (e.g. a typo'd `timout_ms`) are rejected in debug builds and **silently ignored in release builds**. Validation strictness at a trust boundary silently differs by build profile: a mistake caught in dev disappears in production, and the effective default (`timeout_ms` 30 s, delay 0) takes over with no signal. Every other boundary rule (bounds, types, recursion) is unconditional — this one is anomalous.

### Recommended fix

Always validate (`let validate_keys = true;`). Cost is negligible next to the other per-message checks already performed in release.

---

### AUDIT-009 — Silent data loss at teardown: flush/close errors swallowed in destruct paths

- **Severity:** Medium
- **Confidence:** Confirmed (documented tradeoff, but no signal)
- **Domain:** Data integrity / observability
- **Location:** `crates/rabbit-rs-php/src/classes/pool.rs:283-297, 299-309`; `packages/laravel-queue/src/Support/NativePoolFactory.php:80-89`; `packages/laravel-queue/src/RabbitMqQueue.php:404-410`

### Problem

`Pool::close()` and `Pool::__destruct()` flush buffered publications and swallow errors by design ("accepted limitation", commented); re-buffered publications are then lost when the handle drops. `NativePoolFactory::closePools()` catches all throwables silently; `closeConsumers()` catches `NativeException` silently. Net: buffered, already-accepted publications can vanish at GC without any log, metric, or callback. The documented mitigation ("call flush() explicitly") is an invariant no user code reliably maintains.

### Recommended fix

Keep the non-throwing teardown, but surface it: log (once per process), increment a `dropped_unconfirmed_total` counter visible in `stats()`, and fire the backpressure callback. Silent is the problem, not the loss itself.

---

### AUDIT-010 — Event-bridge callbacks discard PHP exceptions and run on the publish hot path

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Observability / Performance
- **Location:** `crates/rabbit-rs-php/src/classes/bridge.rs:112-118, 138-145`; `crates/rabbit-rs-php/src/classes/pool.rs:123, 166, 247`; `crates/rabbit-rs-php/src/classes/consumer.rs:75`

### Problem

1. `let _ = invoke_unlocked(...)` — a user `onConnectionState`/`onBackpressure` callback that throws (or whose callable vanished) fails **invisibly**. The observability feature itself is unobservable.
2. `drain()` is invoked on every `publish()`, `publish_batch()`, `stats()` and `tryNext()`. It calls `connection_states()` (clones the per-broker state map) and `metrics_snapshot()` per call, then may invoke user PHP code. A slow callback directly taxes every publish.

### Recommended fix

Route callback invocation errors to a diagnostic channel (log + counter); cache connection states inside `ClientPool` (watch-based) instead of cloning per drain; document that callbacks must be fast and never throw.

---

### AUDIT-011 — Release pipeline: Packagist token in URL without `--fail`; MIRROR_TOKEN in git remote; force pushes

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Operations / Security
- **Location:** `.github/workflows/release.yml:348-356`, `:423-443`, `:445-453`

### Problem

```yaml
curl -sS -X POST \
  "https://packagist.org/api/update-package?username=goopil&apiToken=${PACKAGIST_TOKEN}" ...
```

- No `--fail`: an HTTP 4xx/5xx exits 0 → Packagist metadata silently not updated after a "successful" release (packages invisible/stale).
- Token in the query string: on any transport error, `curl`'s stderr (which includes the URL) lands in public CI logs.
- `git remote add origin "https://${MIRROR_TOKEN}@github.com/..."` + `push --force` (branch and tag): token in the remote URL of error output; force-push can clobber mirror history silently.
- Contrast: `scripts/update-homebrew-formula.sh:120` at least scrubs output.

### Recommended fix

Use `curl --fail-with-body -o /dev/null -w '%{http_code}'`, pass the token as a request header (`X-Api-Token` is not supported by Packagist — use `--data-urlencode` on a POST body or accept the URL but add `--fail` + `-H 'User-Agent:'`), and never echo it. For the mirror push, use `git -c credential.helper='!f() { echo password=$MIRROR_TOKEN; }; f'` or an `env:`-based HTTPS credential; drop `--force` on tags in favor of a delete+recreate with guard.

---

### AUDIT-012 — Supply chain: release binaries from floating Docker images; composer installer without checksum; no lockfiles

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Security / Operations
- **Location:** `.github/workflows/release.yml:91-95`; `.github/workflows/ci.yml:135-137, 184-186`; root + packages (no `composer.lock` committed)

### Problem

1. Release `.so` artifacts are compiled inside `php:8.4-cli` / `php:8.5-alpine` — **floating tags**. A retagged/compromised upstream image poisons shipped binaries. The lab images, by contrast, are digest-pinned — the project knows the practice and skips it where it matters most.
2. `curl -sS https://getcomposer.org/installer | php` without the documented SHA-384 check (the rustup install two lines above pins its toolchain).
3. No `composer.lock` anywhere; CI runs `rm -f composer.lock && composer update --ignore-platform-reqs` — Laravel CI/coverage builds are unreproducible and platform-req errors are masked.

### Recommended fix

Pin `php:X-cli@sha256:...` / `php:X-alpine@sha256:...` per release-matrix cell; verify the composer installer checksum; commit `composer.lock` for the package (or at least pin in CI with `composer update --lock`).

---

### AUDIT-013 — Core has no logging facility; dead actors leave stale state; `expect` panics in the recovery path

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Observability
- **Location:** `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:327, 367, 192`; `crates/rabbit-rs-core/src/pool/connection_actor.rs:237-241, 265-268, 338-341, 383-386, 413-416`

### Problem

- The only log line in the crate is `eprintln!("recovery generation {generation} failed: {error}")` — discarded under FPM (stderr not captured by default), invisible in Octane.
- The connection actor's all-senders-dropped exits close the connection but never `send_replace(ConnectionState::Closed)` — the watch keeps the last state (`Ready`/`Recovering`) forever; `connection_states()` and the PHP `ConnectionStateChanged` events then lie.
- `wait_for_state`'s `.expect("coordinator actor is alive")` (`recovery_coordinator.rs:192`) and `actor.start().await.expect(...)` (`:327`) turn a dead-coordinator condition into a panic inside a Tokio task (silent recovery death + stale watch) or a panic crossing into `block_on` on the PHP thread.
- Source errors, settlement errors, and blind drops are dropped silently when their bounded buffers fill — documented, but never counted, so `drain_errors()` under-reporting is undetectable.

### Recommended fix

Add `tracing` (optional subscriber; default no-op to keep the extension dependency-lean), replace `eprintln!`, broadcast `Closed` on all actor exit paths, and convert the two `expect`s into typed errors. Add counters for dropped errors.

---

### AUDIT-014 — Consumer acquisition busy-spins the PHP thread when Ready + establish in progress

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Performance
- **Location:** `crates/rabbit-rs-core/src/client.rs:357-388`

### Problem

When the coordinator state is `Ready` but `coordinator.consumer()` fails (typically: `establish_lock` held by another acquisition or by recovery), the acquisition loop does `tokio::task::yield_now()` in a tight circle until `wait_timeout` (default 30 s). This runs under `block_on` on the **PHP worker thread** — 100% CPU burn on that thread for up to 30 s per stuck acquisition, in FPM workers. A dead coordinator's `wait_for_transition` returning `None` is also ignored, prolonging the spin.

### Recommended fix

Wait on the `Notify`/watch of consumer-map changes instead of yielding; fail fast on `None` from `wait_for_transition`.

---

### AUDIT-015 — `ackBatch` validates its 256 cap after settling; partial side effects on error

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Correctness
- **Location:** `crates/rabbit-rs-php/src/classes/consumer.rs:161-172`

### Problem

The cap check (`count >= 256`) runs **inside** the loop: with 257 entries, 256 settlements are enqueued before the exception is raised; a mid-batch error (e.g. `AlreadySettled`) aborts the remaining entries. The caller cannot know how much of the batch was settled — a validation error with partial side effects, exactly what the July audit's "PHP bounds … with precise error paths" item was meant to prevent. `nextBatch` clamps in core (`set.rs:401`), `ackBatch` doesn't.

### Recommended fix

Count/validate `deliveries->count()` before the loop; on mid-batch settlement errors, either continue (error-tolerant) and report, or pre-validate all tokens as `Pending` before settling any.

---

### AUDIT-016 — `pop(null)` bypasses the profile validation used by `pop($queue)`

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Correctness / DX
- **Location:** `packages/laravel-queue/src/RabbitMqQueue.php:358-360`

### Problem

`pop(null)` falls back to `profileForQueue(defaultQueue) ?? defaultQueue` — passing an unknown profile name straight to the native pool, skipping the `hasProfile`/`auto_subscribe` check and the actionable error message that the `$queue !== null` branch produces (`:365-374`). The user gets an opaque native "worker profile not ready / unknown profile" error instead of "No worker profile subscribes to queue … enable auto_subscribe".

### Recommended fix

Route the default-queue branch through the same validation as the named branch.

---

### AUDIT-017 — No-pcntl supervisor fallback breaks after 60 s (Symfony default timeout); `--timeout` doc mismatch

- **Severity:** Medium
- **Confidence:** Confirmed (code + Symfony Process default semantics)
- **Domain:** Correctness / Operations
- **Location:** `packages/laravel-queue/src/Console/WorkerSupervisor.php:160-176, 253-267`; `:19` and `RabbitMqWorkCommand.php:19`

### Problem

`runInline()` (fallback when `pcntl_fork` is absent) blocks in `$process->wait()`. `new Process(...)` never calls `setTimeout()`, so Symfony's **default 60 s timeout** applies: a real `queue:work` child (infinite by design) dies with `ProcessTimedOutException` after one minute on Windows / no-pcntl builds, then gets restarted — an hourly crash-restart loop that looks like flapping. Tests only exercise the path with short-lived stubs. Additionally, `--timeout` is documented as "seconds a child process can run" but is forwarded as `queue:work --timeout` (per-job timeout) — operators setting it to defend against runaway children get job-level semantics instead.

### Recommended fix

`$process->setTimeout(null)` before `start()` in the supervisor path; fix the option doc or pass it as a wall-clock supervision budget.

---

### AUDIT-018 — Nested AMQP headers silently dropped from `Delivery::metadata()`

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Observability / compatibility
- **Location:** `crates/rabbit-rs-php/src/classes/delivery.rs:149`; stub note `stubs/rabbit_rs.stub.php:272`

### Problem

`HeaderValue::Array | Table` are silently omitted — consumers relying on `x-death`, `x-first-death`, or any nested broker structure see nothing, with no error and no marker. For a library that asks users to build idempotent consumers, hiding death-routing metadata silently is a real trap.

### Recommended fix

Expose at least a JSON-encoded string for nested values, or a `headers_raw()` binary representation, and emit a metric when elision occurs.

---

### AUDIT-019 — `stats()` percentiles encode "no samples" as `0`

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Observability
- **Location:** `crates/rabbit-rs-php/src/classes/pool.rs:316-326`

### Problem

`insert_percentile` stores `0` when there are no samples — indistinguishable from "p50 = 0 ms". Dashboards and alerts built on `rabbit-rs:status` / `stats()` will read a quiet system as ultra-fast.

### Recommended fix

Store `null` (or `-1`) for absent percentiles; document it in the stub.

---

### AUDIT-020 — Chaos/at-least-once test infrastructure is orphaned; the most important tests silently skip

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Testing
- **Location:** `packages/laravel-queue/tests/Integration/AtLeastOnceChaosTest.php:10-16` vs `lab/rabbitmq/compose.yaml` (no Toxiproxy service); `docs/plans/2026-08-21` note removing Toxiproxy

### Problem

The chaos suite hardcodes `http://localhost:8474` and a `rabbitmq-1-toxiproxy` container that the lab no longer provisions (Toxiproxy was removed from the compose on 2026-08-21). Every test in the suite `markTestSkipped`s without the extension — and even with the extension, the Toxiproxy-dependent scenarios cannot run as scripted. The project therefore currently has **zero executable chaos coverage** for exactly the property it sells (at-least-once under faults), while the docs claim chaos validation happens "through integration tests with the mock transport and the 3-node lab".

### Verification

`docker compose -f lab/rabbitmq/compose.yaml config | grep -c toxiproxy` → 0 matches.

**Worse, demonstrated during this audit's dynamic verification:** the tests do not skip when a Toxiproxy answers on 8474 — they inject toxics into *whatever* answers. On this machine, another project's containerized Toxiproxy (`feat-toxiproxy-*`, proxying Redis instances on 6380–6382) was listening on 8474. The chaos scenarios ran "green", injecting `latency`/`timeout`/`bandwidth` toxics into a Redis proxy of an unrelated application (toxics visible via `curl localhost:8474/proxies`), then asserted their internal `missing = 0` invariant — a vacuous PASS that validates nothing about Rabbit RS fault behavior. The suite therefore has two failure modes: silent skip (no proxy) and **false positive via foreign proxy** (any proxy on the port). Both are invisible in CI output.

### Recommended fix

Re-add a Toxiproxy service (digest-pinned) to the compose with the profile names the tests expect, bind it to a lab-unique port, and have the suite verify the proxied targets are the lab's RabbitMQ nodes (proxy list assertion) before running — or rewrite the chaos tests to use the lab's native failure levers (docker stop/pause, node restart) and delete the proxy assumptions. Either way, make test-integration.sh fail loudly when chaos tests skip unexpectedly.

---

### AUDIT-021 — Documentation drift on the declared "sources of truth"

- **Severity:** Medium
- **Confidence:** Confirmed
- **Domain:** Maintainability
- **Location:** `docs/plans/2026-07-30-rabbitmq-native-design.md:309-326` vs `.github/workflows/release.yml:79-83` + `composer.json:13`; `README.md:5-6` + `sonar-project.properties:11-13` vs `Cargo.toml:13`

### Problem

1. The design doc still promises "NTS **and ZTS**" and a "16 release archives" matrix; reality is 8 NTS archives with `support-zts: false` (CHANGELOG documents the drop; the design doc was never updated — and AGENTS.md designates this file as a source of truth).
2. Mixed repository slugs: README badges and Sonar links say `Goopil/php-rabbit-rs`; Cargo, CHANGELOG, release scripts say `Goopil/rabbit-rs`. One is wrong → broken badges or misdirected automation.
3. The design example asset name lacks the `v` prefix used by the actual pipeline (`php_rabbit_rs-v1.2.0_...`).

### Recommended fix

One-hour doc pass: update the design doc's Distribution section to match `docs/distribution.md` (which is correct), fix the slug, mark superseded design claims with dated notes like the existing `max_in_flight` note.

---

### AUDIT-022 — One real-sleep timing-sensitive test in an otherwise paused-time suite

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Testing
- **Location:** `crates/rabbit-rs-core/tests/consumer.rs:463-497` (`sleep(500ms)` at `:484`, plain `#[tokio::test]`)

### Problem

The suite's discipline is `start_paused = true` + injectable clocks (and the AGENTS.md forbids real sleeps in unit tests). This one test uses a real 500 ms sleep and can flake under load, and it's the kind of test that erodes trust in CI on slow runners.

### Recommended fix

Convert to paused time with the mock transport (the surrounding tests show the pattern).

---

### AUDIT-023 — Blind mode bypasses the byte budget; silent drops with no metrics

- **Severity:** Low
- **Confidence:** Confirmed (documented contract)
- **Domain:** Performance / Observability
- **Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:214-228`; `crates/rabbit-rs-core/src/publisher/pump.rs:203-214`

### Problem

`publish_blind` bypasses semaphore and `ByteBudget` (64 MiB global cap); the blind pump bounds only message *count* (in-flight cap `2×capacity`, max 128) with unbounded per-message size, and drops jobs silently on channel clear/transport error — zero counters. A caller using blind mode has a different (weaker) memory contract than the safe path and cannot detect drops.

### Recommended fix

Count blind drops in `Metrics` (one atomic) and document the per-message-size ceiling; consider a blind-specific byte cap.

---

### AUDIT-024 — Silent data-elision cluster (headers, early-ack, attempts headers)

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Observability
- **Location:** `crates/rabbit-rs-core/src/consumer/actor.rs:233-241` (early-ack result discarded); `crates/rabbit-rs-core/src/transport/lapin.rs:625` (decimal headers → `None`); `crates/rabbit-rs-core/src/consumer/attempts.rs:82-88` (malformed attempt headers treated as absent)

### Problem

Three independent spots where data is dropped without signal: a failed early-`ack` is neither metriced nor surfaced (with `early_ack` the delivery is already presented as `AutoAcked`, so the only recovery — broker redelivery — is invisible even to metrics); `AMQPValue::DecimalValue` header values become `None`; malformed attempt counters are ignored. Each is small; together they define a pattern that makes odd production behaviors undiagnosable.

### Recommended fix

Record early-ack failures as settlement errors; keep decimal values as binary/string; log-or-count malformed headers once per subscription.

---

### AUDIT-025 — TLS cert paths not validated until connect time

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Operations / DX
- **Location:** `packages/laravel-queue/src/Config/ConfigNormalizer.php:178-191`; `crates/rabbit-rs-core/src/config.rs:414-475`

### Problem

`ca_cert`/`client_cert`/`client_key` are accepted as arbitrary strings at normalization (and only hashed into the fingerprint in the core); failure surfaces at first connect as a transport error naming the path. A typo'd path is discovered at runtime, in production, on the first publish. The transport does fail *loudly and permanently* (good — `transport_tuning.rs` covers unreadable files), but validation-time detection would be cheap and strictly better.

### Recommended fix

Add existence/readability checks in `ConfigNormalizer` (path-addressed errors) — the core `TransportError::config` path already produces the right message shape.

---

### AUDIT-026 — Lab exposes an administrator-tagged RabbitMQ on `0.0.0.0` with weak defaults

- **Severity:** Low
- **Confidence:** Confirmed (lab-only, self-documented)
- **Domain:** Security
- **Location:** `lab/rabbitmq/rabbitmq/rabbitmq.conf:1,5` (`loopback_users.guest = false`, management listener `0.0.0.0`), `lab/rabbitmq/rabbitmq/definitions.json:8-17` (`admin/admin_lab`, `rabbit_rs/rabbit_rs_lab`), `compose.yaml:45-123` (host-published ports), `compose.yaml:8` (fixed Erlang cookie)

### Problem

Acceptable for a local lab and annotated as such (`rabbitmq.conf:25-26`), but anyone running `lab-up.sh` on an untrusted network exposes management + AMQP with administrator credentials. The fixed Erlang cookie additionally would allow node takeover if reachable.

### Recommended fix

Bind management to `127.0.0.1` by default (compose port mapping `127.0.0.1:15672:15672`), keep the note.

---

### AUDIT-027 — Credential-bearing URI is `pub` and handed to lapin; leak surface depends on lapin error strings

- **Severity:** Low
- **Confidence:** Likely
- **Domain:** Security
- **Location:** `crates/rabbit-rs-core/src/transport/lapin.rs:361-388` (`pub fn connection_uri`), `:645-663` (`map_lapin_error` stringifies lapin errors)

### Problem

The AMQP URL embeds `user:password` (`:373-376`). rabbit-rs never `Display`s the URL itself and the secret-redaction suite is thorough (`tests/Extension/SecretsTest.php`, `tests/Unit/ConfigNormalizerTest.php:238`), but `connection_uri` is a **public** function returning a credential-bearing `Url`, and every lapin error is propagated verbatim into `TransportError` → connection state reasons → PHP exceptions → `rabbit-rs:status` output. If any lapin error string ever embeds the URI (version change, `Url` display in a future path), credentials reach logs. The July audit flagged this (#9); the mitigation (no Display in rabbit-rs code) reduced but did not remove the surface.

### Verification

Fuzz `map_lapin_error` with lapin error variants; grep lapin's source for URI-in-error usage; decide whether to pass credentials out-of-band (lapin supports `ConnectionProperties::with_credentials`... verify against lapin 4.x API) and keep the URI credential-free.

### Recommended fix

Make `connection_uri` `pub(crate)`; build the URI without the password and pass credentials via lapin's connection properties if the API allows; add a unit test asserting no `TransportError` payload contains `Credentials::new(...)` values.

---

### AUDIT-028 — Coverage CI ignores test failures but uploads coverage

- **Severity:** Low
- **Confidence:** Confirmed
- **Domain:** Testing / Operations
- **Location:** `.github/workflows/coverage.yml:105` (`php ... vendor/bin/pest || true`), `:109` (`cargo llvm-cov --no-run ... || true`)

### Problem

A red extension test suite still produces a green coverage upload. Combined with `fail_ci_if_error: false` on Codecov, the coverage pipeline can report healthy numbers for a broken build.

### Recommended fix

Run tests strictly; capture coverage separately; let coverage jobs consume the same test artifacts as the CI gate.

---

### AUDIT-029 — Workflow nits: `brew trust` may not exist; `verify-pie-install` over-permissioned

- **Severity:** Low
- **Confidence:** Confirmed (documentation check)
- **Domain:** Operations
- **Location:** `.github/workflows/homebrew-formula-test.yml:23,38`; `.github/workflows/release.yml:288-289`

### Problem

`brew trust goopil/rabbit-rs` is not a documented Homebrew command — if absent on runners, every paths-matching PR fails the formula job. `verify-pie-install` declares `contents: write` although it only downloads and checks (least-privilege violation; the file otherwise has a good per-job permission hygiene and no top-level default).

### Recommended fix

Replace `brew trust` with the supported tap mechanism (`brew tap goopil/rabbit-rs <url>`) and drop `contents: write` from the verify job; add a top-level `permissions: contents: read` to release.yml for defense in depth.

---

### AUDIT-030 — No deadline enforcement for in-flight publishes in Ready phase

- **Severity:** Low
- **Confidence:** Confirmed (code); impact bounded by heartbeat detection
- **Domain:** Resilience
- **Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:434-443` (`next_deadline()` returns `None` in `Ready`), `:709-729` (`publishing` entries only resolve via future completion)

### Problem

A publish handed to a channel whose TCP write never completes sits in `state.publishing` forever, pinning its semaphore permit and byte budget; `wait_for_deadline` only fires in `Suspended`. The stall is bounded by heartbeat dead-connection detection (default 30 s heartbeat → transport error → suspend → replay), so this manifests as a bounded throughput dip rather than a permanent leak — but with heartbeat disabled by an operator, it becomes an unbounded publisher stall with no deadline expiry.

### Recommended fix

Track a deadline per `publishing` entry (same `min(request.deadline, confirm_timeout)` rule) and expire from the select loop in `Ready` as well.

---

### AUDIT-031 — `size()` and `clear()` ignore the publish buffer; purge is not a barrier

- **Severity:** Medium
- **Confidence:** Confirmed (code reading + reproducible test failure on the live lab)
- **Domain:** Correctness / Data integrity
- **Location:** `crates/rabbit-rs-php/src/classes/pool.rs:253-263` (`size`), `:266-276` (`clear`), `crates/rabbit-rs-php/src/classes/publish_buffer.rs:25,49,76-83,195`

### Problem

`Pool::size()` queries the broker (`client.queue_size`) and `Pool::clear()` purges the broker queue without flushing the application-side publish buffer first. On a fresh pool the buffer's flush triggers are both dormant: the count trigger needs 64 buffered messages (`BUFFER_THRESHOLD`), and the 1 ms interval trigger cannot fire because `last_flush` is `None` until the first `flush_all` (`publish_buffer.rs:49`, deliberately per the comment at `:72-75`). The first ≤63 `publish()` calls therefore sit in process memory: `size()` reports 0 for messages the pool accepted (message ids returned), and `clear()` purges the queue while accepted publications remain buffered — they are flushed later and re-populate the very queue the caller asked to purge. Purge-and-fill patterns (test isolation, admin draining, "clear then republish" flows) get silently inverted ordering; observability reads the wrong number. `close()` and consumer `pop()` paths do flush (pool.rs:288, flush_nonempty), which is exactly why this only bites `size`/`clear` — the two operations whose entire value is a truthful, immediate view of broker state. In blind mode the divergence window is unbounded (no threshold flush either).

### Verification

- Laravel integration test `it increases size after push` fails reproducibly on the live lab: 2 `publish()` calls return message ids, `size()` returns 0 (`Failed asserting that 0 is equal to 2 or is greater than 2`).
- Code path confirmed: `pool.rs:117-121` enqueues and only flushes via `should_flush()`; `pool.rs:253-263`/`:266-276` never touch the buffer.

### Recommended fix

One-line-class fixes at the trust boundary of the two methods: call `flush_all()?` at the top of `size()` and `clear()` (same pattern `publish_batch` already uses via `flush()`). Add an integration assertion: `publish → clear → size() == 0` and `publish → size() == count`.

---

## 5. Security (red team summary)

Full findings: AUDIT-011, AUDIT-012, AUDIT-025, AUDIT-026, AUDIT-027. Attack-surface review:

- **Trust boundary (PHP → Rust)**: strong. Bounded everything (256 msgs / 1 MiB / 128 headers / 64 KiB / 24 h), recursion and identity-set recursion guards, non-finite rejection, resources/objects rejected, `deny_unknown_fields` on config structs. One exception: unknown publish keys ignored in release (AUDIT-008).
- **Secret hygiene**: good. `Credentials`/`ConnectionKey`/`ConnectionHandle` Debug redacted (`config.rs:66-74`, `pool/key.rs:9-13`); fingerprint hashes rather than stores; dedicated redaction tests at every surface (`SecretsTest`, `ConfigNormalizerTest:238`, status-command assertions `RabbitMqStatusCommandTest:57-68`). Residual: AUDIT-027 (URI), and the CI-level token handling (AUDIT-011).
- **Rust threads never touch Zend values**: verified — callbacks live in `CallbackSlot` on the PHP side, invoked from `drain()` on the PHP thread only; `#[expect(clippy::arc_with_non_send_sync)]` documents the confinement (`bridge.rs:36-39`).
- **Fork safety**: two-layer PID guards plus registry-level invalidation with `mem::forget` of the inherited runtime (`runtime.rs:227-236`) — correct (Tokio worker threads do not survive fork); real `pcntl_fork` tests exist (`ForkInvalidationTest`).
- **Supply chain**: Rust side exemplary (crates.io-only, no git deps, no patches, `cargo-deny` with yanked=deny, SHA-pinned actions, digest-pinned lab images, SLSA attestations verified in-pipeline). Composer/CI side: AUDIT-012.
- **Injection surfaces**: AMQP strings are length-checked AMQP long strings; headers flat-only with integer keys rejected; no SQL/shell/command surfaces in the driver (supervisor builds argv arrays, not shell strings — `WorkerSupervisorTest` covers propagation).
- **Not applicable by design**: no HTTP endpoints, no user-facing web surface, no deserialization of untrusted PHP objects (payloads are opaque bytes end-to-end).

No exploitable vulnerability was identified in shipped code. The weakest links are the release pipeline's secret handling and the un-pinned release base images.

---

## 6. Performance & scalability

Measured architecture facts (this audit's reading):

- **One runtime worker thread** by default (`runtime.rs:45-51`) hosts every actor, pump, and I/O task. Deliberate I/O-bound tradeoff; the CPU-bound conversion work happens on PHP threads. Risk: a misbehaving actor (e.g. the AUDIT-014 spin if it ever lands on the runtime, or a slow TLS file read during connect — `lapin.rs:312-352` does blocking `std::fs::read` **inside** async connect) stalls all brokers' progress for the duration. With one worker, a 10 ms disk hiccup during TLS connect delays every other actor's timers.
- **Per-publish overhead**: `bridge.drain()` per publish (AUDIT-010) clones the connection-state map and snapshots metrics; at 50k msg/s this is measurable. Move to a change-notification model.
- **Scheduler**: O(n) per dispatch with `eligible`/`contains` Vec scans (`scheduler.rs:124-145`) — fine for n≤50 subscriptions, would degrade at 1000+.
- **Bounded memory**: publisher 1024 commands + 64 MiB bytes (safe path); consumer per-subscription `max_buffered_bytes` (64 MiB default) + `pending_incoming` bounded by prefetch; blind path count-bounded only (AUDIT-023). At 10× traffic, backpressure surfaces as `BackpressureException` at the PHP layer — correct shape.
- **The 3 a.m. scaling question**: the limiter is not throughput, it's the AUDIT-001 stall — losing 2% at any scale is the real ceiling for adoption.
- **Benchmarks**: `benchmarks/driver-bench` exists with documented methodology and honest loss accounting (it's where AUDIT-001 was found — credit where due), but nothing in CI enforces an anti-regression budget. The design calls for "anti-regression budgets recorded alongside comparative gains" — not wired.

---

## 7. Reliability & data integrity

- **Delivery guarantee, achieved parts**: replay with same `message_id` + original deadline under repeated loss (`tests/publisher.rs:929`); mandatory-return precedence over ACK (`tests/publisher.rs:273`); stale-generation ack rejection (`tests/recovery.rs:216`); deterministic recovery order asserted (`tests/recovery.rs:304`); `Release(delay)` = publish→confirm→ack with original left unacked on failure (`tests/consumer.rs:561-659`).
- **Delivery guarantee, broken parts**: AUDIT-001 (silent misses), AUDIT-002 (poison loop), AUDIT-003 (delay timing), AUDIT-009 (teardown loss without signal), AUDIT-006 (topology silently not declared).
- **"Success returned while the operation isn't done"** (the protocol's Phase 7 question): yes, by design in three spots — buffered publish accepted before flush (documented at-least-once boundary), fire-and-forget settlements (errors surface on the next `pop()` via `drainSettlementErrors`, or never if the process ends), and `close()`/`__destruct` swallowing (AUDIT-009). All three are process-local at-least-once semantics — defensible — but only the first is documented loudly enough.
- **Failure-mode gaps**: consumer-establishment blast radius (AUDIT-007); dead-actor stale state (AUDIT-013); lost Close on drop (AUDIT-005).
- **Recovery**: backoff 100 ms→30 s, equal jitter 80–100%, permanent classification for auth/protocol (403/530) — matches design. Retry storms: bounded by per-broker coordinator, no cross-broker amplification observed.

---

## 8. Architecture

**Strengths (keep):** three-layer split with the core runtime-independent; lapin strictly behind `Transport` (mock scriptable, feature-gated real-broker tests); actor-per-broker with generation-tagged everything; composite consumer fan-in with per-source retirement; `#![forbid(unsafe_code)]` held across both crates; typed errors with actionable paths at config boundaries; bounds as a first-class design element.

**Structural risks:**

1. **Observability was never given a home.** No `tracing`, no metrics export, one `eprintln!`. The design promised "logs are structured and never contain secrets" — there are no logs. This is the architectural decision that will hurt most in production (AUDIT-013).
2. **The event bridge inverts control in the hot path** (user PHP callbacks invoked per publish/next) — acceptable for V1, worth isolating behind a buffered queue before marketing the latency story.
3. **Core defaults drift from design silently** — `max_in_flight` removed (documented), plugin detection dropped (AUDIT-004), 16→8 assets (documented elsewhere). Recommend a short "design deltas" ledger rather than scattered dated notes.
4. **The Laravel layer is the most mature layer** (247 tests, thorough normalizer) — the weak siblings are core observability and the consumer dispatch correctness (AUDIT-001).

---

## 9. Testing

**Coverage reality:** 275 Rust + 247 PHP (unit/feature) + 23 Rust integration + 8 real-broker Laravel integration (when the lab is up) + 9 PHPT + FPM lab. Deterministic paused-time discipline, injectable clock/jitter, scriptable mock with operation gates. Genuinely good.

**What bugs could be introduced today without any test failing?**

1. The AUDIT-001 stall class (idle consumer + fill) — no test covers it; the bench masks it with reconnection.
2. `delay.mode = ttl` over-bucket behavior (AUDIT-003) — no test on the publisher fallback path.
3. `DelayMode::Auto` semantics (AUDIT-004) — no test asserts plugin detection because none exists.
4. Any change to the actor-exit paths (stale watch state, AUDIT-013) — no test asserts `Closed` broadcast because it doesn't happen.
5. `ackBatch` cap semantics (AUDIT-015) — only the happy path is asserted.
6. Release-side: a broken Packagist trigger (`--fail` missing, AUDIT-011) — release "succeeds".
7. Coverage-side: a fully red extension suite still uploads green coverage (AUDIT-028).
8. The chaos suite can rot silently (AUDIT-020) — skips are invisible to CI, and a foreign process on port 8474 turns them into false positives (demonstrated during this audit).

**Missing suites:** property-based tests for the scheduler/ledger/confirm ledger (the July audit already recommended proptest); a deterministic idle-consumer-then-burst test; supervisor wall-clock tests with long-lived stubs; TLS integration (only unit-level URI assertions today).

---

## 10. Operations

- **Release pipeline is genuinely strong**: draft → 30 assets (10 ZIP + SHA256 + SBOM) → `gh attestation verify` on each → PIE end-to-end install check → publish → mirror split. The weakest links are secret handling and image pinning (AUDIT-011/012).
- **Incident path (03:00)**: a Rabbit RS outage produces no logs (AUDIT-013), metrics only on demand via `rabbit-rs:status` (catches Throwable and prints the message, AUDIT-027-adjacent), percentiles that read 0 when empty (AUDIT-019), and settlement errors logged only if a container logger exists. The lab ships Prometheus, but the library exposes no scrape endpoint. Answer to "can a team identify quickly what is happening?" — **no**.
- **Rollback**: extension + package version-synced, PIE verified, draft-first — good. Queue topologies are compatible across versions (idempotent declare). Old-version workers and new-version workers coexist safely (process-local registries, competing consumers).

---

## 11. Scores

| Domain | Score /10 | Rationale |
|---|---|---|
| Correctness | 6 | Recovery/confirm machinery sound; dispatch-path bugs (attempts, delay fallback) and cap validation gaps |
| Architecture | 8 | Clean layering, transport abstraction, generation model; observability never designed in |
| Performance | 6 | Bounded and coherent; per-publish drain tax, single worker thread, no CI-enforced budgets |
| Scalability | 7 | Bounded structures hold at 10×; per-broker actors parallelize; scheduler O(n) fine at realistic n |
| Stability | 6 | Recovery solid; leak paths (lost Close), stale state, blast radius |
| Resilience | 7 | Backoff/jitter/permanent classification right; local-error→global-teardown coupling |
| Security | 7 | Excellent boundary hardening and redaction; release pipeline secret handling and image pinning |
| Data integrity | 5 | The contract is the product; AUDIT-001/002/003/009 each violate it in a distinct way, AUDIT-031 breaks purge/stats truthfulness |
| Testing | 7 | Strong deterministic suites; blind spots exactly where the contract lives; chaos coverage rot |
| Maintainability | 7 | Consistent conventions, docs drift on sources of truth, two audit conventions |
| Observability | 4 | No logs, no export, 0-ms ambiguity, silent elisions — the defining weakness |
| Operations | 6 | Strong release engineering; silent failure modes and no runbook for poison loops |
| Developer experience | 7 | Scripts (`check.sh`, lab, coverage) are exemplary; local bootstrap requires care; PHP 8.5 works locally |
| **Overall** | **6** | Strong engineering culture; the delivery contract it advertises is not yet certified |

---

## 12. Risk matrix

| Finding | Probability | Impact | Severity | Confidence | Priority |
|---|---|---|---|---|---|
| AUDIT-001 idle-consumer delivery loss | Medium (prod pattern) | Critical (silent loss) | **Critical** | Confirmed (observed) / root cause NV | P0 |
| AUDIT-002 poison loop + dead `--tries` | High (throwing job) | High (worker exhaustion) | **High** | Confirmed | P0 |
| AUDIT-003 TTL delay → immediate publish | Medium (ttl users, delay > 120 s) | High (wrong business timing) | **High** | Confirmed | P0 |
| AUDIT-004 `auto` delay mode lie | Medium (plugin-less installs) | Medium (loud failure / no fallback) | Medium | Confirmed | P1 |
| AUDIT-005 lost Close → channel leak | Low per event, cumulative | Medium (FD leak, stuck unacked) | Medium | Confirmed | P1 |
| AUDIT-006 topology fallback silent | Low | Medium (DLQ silently absent) | Medium | Confirmed | P1 |
| AUDIT-007 establish failure nukes connection | Medium | Medium (recovery storms) | Medium | Confirmed | P1 |
| AUDIT-013 no logging + stale state | High (any incident) | High (blind 3 a.m.) | **High** | Confirmed | P1 |
| AUDIT-011 release token handling | Low per release | High (token leak / silent release) | Medium | Confirmed | P1 |
| AUDIT-012 supply chain (images, lock) | Low | High (poisoned artifacts) | Medium | Confirmed | P1 |
| AUDIT-020 chaos coverage rot | Already true | High (no fault coverage) | Medium | Confirmed | P1 |
| AUDIT-031 `size`/`clear` bypass publish buffer | Medium (≤63 first publishes; purge-and-fill flows) | Medium (wrong stats, inverted purge ordering) | Medium | Confirmed (repro) | P1 |
| AUDIT-014 acquisition spin | Medium (concurrency) | Low (CPU burn, bounded) | Medium | Confirmed | P2 |
| AUDIT-008 debug-only validation | Medium | Medium (silent misconfig) | Medium | Confirmed | P1 |
| AUDIT-009 teardown loss silent | Medium (GC paths) | Medium (bounded loss, no signal) | Medium | Confirmed | P2 |
| AUDIT-010 callback exceptions discarded | Low | Low–Medium | Low | Confirmed | P2 |
| AUDIT-015 ackBatch partial settle | Low | Low–Medium | Low | Confirmed | P2 |
| AUDIT-017 supervisor 60 s timeout | Low (no-pcntl only) | Medium (crash-restart loop) | Medium | Confirmed | P2 |
| AUDIT-030 Ready-phase no deadline | Low (heartbeat bounds it) | Low | Low | Confirmed | P3 |
| AUDIT-016/018/019/021/022/023/024/025/026/027/028/029 | — | — | Low | Confirmed | P3 |

---

## 13. Remediation plan

### Phase 0 — Immediate (contract integrity)

- [ ] Fix AUDIT-001 — repro (mock test: idle consumer + burst), root-cause hunt in the delivery pipeline, remove the bench workaround once green.
- [ ] Fix AUDIT-002 — enforce MaxAttempts at dispatch (typed terminal error; reject/requeue policy decision), return true attempts, add dispatch-path test.
- [ ] Fix AUDIT-003 — fail the publish on delay-routing error; validate max delay at config load.
- [ ] Fix AUDIT-008 — always validate publish keys (one line).
- [ ] Fix AUDIT-011 — `--fail` on both curl calls; remove token from remote URL; no force-push on tags.

### Phase 1 — Stabilisation

- [ ] AUDIT-013 — add `tracing` (no-op default), broadcast `Closed` on actor exits, replace the two `expect`s, count dropped errors.
- [ ] AUDIT-004 — implement plugin detection with bounded timeout or relabel `auto` → `plugin` in docs + config comment.
- [ ] AUDIT-005 — actor-owned sender pattern so external-handle drop closes the set; add drop-close test with a full command channel.
- [ ] AUDIT-007 — scope establishment retries to the profile before escalating to `connection_lost`.
- [ ] AUDIT-020 — restore executable chaos coverage (Toxiproxy back in compose, or lab-native failure levers); make unexpected skips fail CI.
- [ ] AUDIT-012 — pin release base images by digest; verify composer installer checksum; commit `composer.lock` for the package.
- [ ] AUDIT-015 — validate `ackBatch` size before settling; pre-check token states.
- [ ] AUDIT-031 — flush the publish buffer at the top of `size()` and `clear()`; add the `publish → clear → size() == 0` integration assertion.
- [ ] AUDIT-017 — `setTimeout(null)` on supervisor children; fix `--timeout` docs.

### Phase 2 — Performance & observability

- [ ] AUDIT-010 — change-notification for connection states; callback errors surfaced; document callback contract.
- [ ] AUDIT-014 — wait on consumer-map changes instead of yield-spinning.
- [ ] AUDIT-019 — `null` percentiles when empty.
- [ ] AUDIT-023 — blind-drop counter; documented blind memory ceiling.
- [ ] Wire the bench into CI as an anti-regression budget (the design already calls for it).

### Phase 3 — Architecture (decisions, not urgency)

- [ ] Decide the metrics-export story (Prometheus adapter vs stats()-only) — the lab already scrapes nothing today.
- [ ] Audit AUDIT-027's lapin error-string surface and move credentials out of the URI if the API allows.
- [ ] AUDIT-021 — one doc pass to reconcile design ↔ reality (ZTS, assets, slugs, delay auto).

### Phase 4 — Long term

- [ ] Property-based tests (scheduler fairness, ledger contiguity, replay ordering).
- [ ] TLS integration scenario in the lab (amqps listener + client certs).
- [ ] AUDIT-024/016/018/022/025/026/028/029/030 as opportunity work.

**Do NOT fix** (deliberate non-recommendations):

- **Single runtime worker thread** — a deliberate, defensible tradeoff for an I/O-bound PHP embed; change only with benchmark evidence of actor CPU saturation.
- **Fire-and-forget settlement API** — coherent with the pull-based delivery model; fix error *surfacing*, don't make PHP wait on acks.
- **`mem::forget` on forked runtime** — correct fork-safety strategy, not a leak to fix.
- **Buffered-publish-before-flush semantics** — documented at-least-once boundary; do not turn every publish into a confirmed RPC by default (that's what `publish_batch` + Safe mode are for).
- **The `establish_lock` serialization** — recovery correctness over parallelism; revisit only with evidence of contention at scale.
- **The 8-asset NTS matrix vs the design's 16** — a documented, justified product decision; update the stale doc, don't rebuild ZTS.

---

## 14. Final verdict

**Rabbit RS is a well-engineered system whose documentation, discipline and release engineering are ahead of its delivery guarantees.** The previous audits were taken seriously and every one of their findings held under re-inspection as fixed. But this audit's central discovery is uncomfortable: the project's own benchmark has verified a silent delivery-loss pattern (~2%) and routed around it with reconnects, rather than treating it as the P0 contract violation it is. Combined with a poison-message infinite loop by default and a delayed-delivery path that can fire immediately, the "no silent loss" promise — the reason this library would be chosen over php-amqplib — is not yet certified.

The path to certification is short and known: five Phase-0 items, the observability floor (AUDIT-013), and executable chaos coverage (AUDIT-020). The bones are excellent.

**Three things I would fix tomorrow:**

1. **AUDIT-002** — dispatch-path attempts enforcement (small diff, removes an infinite loop class).
2. **AUDIT-003 + AUDIT-008** — fail-loudly on delay-routing errors; always-on key validation (two small diffs, two silent behaviors gone).
3. **AUDIT-001** — write the failing repro test for the idle-consumer stall and start the investigation; nothing else matters if this one is real in production shapes.

---

*Audit trail: 275 Rust tests, 247+620 PHP assertions, 23 integration tests executed during this audit on a live 3-node lab; 12 previous-audit findings re-verified as fixed; 31 new findings, all with code-verified evidence (AUDIT-031 and AUDIT-020's false-positive mode discovered dynamically).*

**Re-audit directive:** to re-run this audit later, keep `AUDIT-XXX` identifiers stable: *"Re-audit the project. Compare with docs/audit/2026-08-31-red-team-audit.md. Identify resolved findings, regressions, new risks, and changed severities."*
