# Technical Audit — rabbit-rs

**Date:** 2026-08-31 · **Target:** `origin/main` @ `b06b62f` · **Method:** static review in 7 passes (architecture → 5 parallel adversarial passes → fresh-eyes red team → personal verification of every cited location) + full local test run (`cargo nextest --workspace --all-targets`: 275 passed).

Confidence labels: **CERTAIN** (demonstrable from code), **PROBABLE**, **NEEDS-VERIFICATION**.

---

## 1. Verdict: 5.5/10

The engineering discipline is real: typed errors everywhere, explicit bounds, `#![forbid(unsafe_code)]` upheld, secrets redacted, docs that honestly state in-memory limits, tests that use paused Tokio time. The individual modules are well built.

The system fails at the seams. The product's headline promise — *automatic reconnection after an outage* — has **no trigger in production code**: nothing ever detects connection loss, so recovery machinery that is carefully tested with hand-injected events never runs against a real failure. On top of that: CI compiles out every real-broker test while booting a RabbitMQ lab, several paths can silently drop or mis-time messages, and the configuration surface has drifted from actual behavior.

**What prevents a better grade:**

1. No production path detects connection loss (F-01): a routine broker restart permanently stops consumption and bricks publishing in every PHP process until process restart, with zero monitoring signal.
2. CI never executes a real-broker test (F-10): the delivery-critical contract is validated only against a mock transport and hand-fed recovery events.
3. A family of "suspension without wake-up" bugs (F-02, F-03) bricks publishers on recoverable channel-level failures.
4. Poison-message handling converts permanent refusals into infinite pending redelivery loops instead of terminal settlements (F-07, F-11).
5. Silent-loss paths exist in dead-letter wiring (F-05), delay routing (F-06), and pool teardown (F-18).

**Top short-term risks:** broker restart → fleet-wide silent outage; delayed jobs on a plugin-less broker → total publish outage per process; `no_ack` consumer → OOM; supervisor + `--max-jobs` → self-inflicted fleet stop; rolling config change of delay buckets → `PRECONDITION_FAILED` publish outage.

**Top long-term risks:** CI validating mocks, not the product, lets every seam regression through; config surface and behavior keep diverging as knobs are added; unbounded synthesized topology on the broker (orphan delay queues, no GC); observability gaps (per-process metrics, `eprintln!`-only failures) make incidents undiagnosable at 3am.

---

## 2. Findings table

| # | Prio | Domain | Problem | Confidence | Impact | Action |
|---|------|--------|---------|------------|--------|--------|
| F-01 | P0 | Core/recovery | No production connection-loss detection; recovery never triggers | CERTAIN | Availability, silent outage | Transport liveness signal → `connection_lost` |
| F-02 | P1 | Core/publisher | `delay.mode=auto` = plugin; declare failure suspends publisher forever | CERTAIN | Publish outage per process | Probe plugin / fail message terminally |
| F-03 | P1 | Core/publisher | `Ready` event error swallowed; failed `enable_confirms` leaves publisher suspended, generation consumed | CERTAIN (swallow) / PROBABLE (trigger) | Publish outage | Propagate error, retry generation |
| F-04 | P1 | Core/consumer | `pending_incoming` unbounded in `no_ack`; documented guard never validated | CERTAIN | OOM | Enforce `no_ack ⇒ early_ack` or bound |
| F-05 | P1 | Core/topology | DLQ binding emitted only for first subscription sharing a DLQ | CERTAIN | Silent poison loss | Binding per (dlq, routing_key) pair |
| F-06 | P1 | Core/delay | Delay > max TTL bucket: published immediately (consumer path: hot loop) | CERTAIN | Wrong-time execution | Validate delay at boundary |
| F-07 | P1 | Core/attempts | Resolve error swallowed (`attempts=2`), cap 20 hard-coded, capped delayed release → pending hot loop; Laravel `--tries>20` void | CERTAIN | Poison loops, retry contract | Configurable cap + terminal settle |
| F-08 | P1 | Bridge | One-shot `SourceReplaced` + PHP consumer cache never evicted → worker stops consuming after recovery | CERTAIN | Silent per-worker stop | `unset($this->consumers[$profile])` on error |
| F-09 | P1 | Laravel/supervisor | Clean exits (e.g. `--max-jobs` recycling) count as crashes; fleet stops after `--max-restarts` | CERTAIN | Self-inflicted outage | Distinguish exit 0 / healthy runtime |
| F-10 | P1 | CI | Real-broker suites compiled out of CI (`--features integration` missing); Laravel chaos suite never invoked; lab started for nothing | CERTAIN | Regression blindness | Add feature flag + run integration script |
| F-11 | P1 | Laravel/job | Invalid JSON throws in `marshalJob` before settlement → unkillable message, queue stall | CERTAIN | Queue-wide stall | Reject(requeue=false) on unmarshable payloads |
| F-12 | P2 | Core/pool | `wait_for_state` panics on dead coordinator; crosses FFI as abort | CERTAIN | Process abort (narrow race) | Return `Option`, map to `ClientError::closed` |
| F-13 | P2 | Core/client | `size`/`purge` connections bypass recovery, cached forever; 2nd AMQP conn per vhost | CERTAIN | Permanent admin-op failure | Route through coordinator or evict on error |
| F-14 | P2 | Core/publisher | Blind mode bypasses byte budget (64 MiB bound unenforced); utilization metric reports 0 | CERTAIN | Memory bound violation | Reserve budget in `publish_blind` |
| F-15 | P2 | Core/client | `publish_batch` discards already-accepted waiters when a later broker fails | CERTAIN | Outcome ambiguity | Await collected waiters before erroring |
| F-16 | P2 | Core/consumer | `Drop` close is lossy `try_send`; actor + broker subscriptions leak when channel full | CERTAIN | Resource leak | Dedicated close signal |
| F-17 | P2 | PHP ext | Callback exceptions destroyed (`let _ = invoke`); single-slot callbacks stolen by 2nd connection with same fingerprint | CERTAIN | Silent observability death | Rethrow; multi-slot registry |
| F-18 | P2 | PHP ext | `close()`/`__destruct` flush: errors swallowed, deadline-expired publications silently dropped; destruct can block up to `timeout_ms` (30 s default, 24 h ceiling) | CERTAIN | Silent loss; FPM stalls | Fixed shutdown budget + drop counter |
| F-19 | P2 | PHP ext | Publish key validation `cfg!(debug_assertions)` — release silently ignores typos (`delay_ms` typo → immediate publish) | CERTAIN | Dev/prod divergence | Always validate |
| F-20 | P2 | PHP ext | `ackBatch` throws after 256 side effects already enqueued | CERTAIN | Ambiguous semantics | Pre-check length |
| F-21 | P2 | PHP ext | `Consumer::next` fast path skips `bridge.drain()` → state callbacks starve under steady traffic | CERTAIN | Misleading telemetry | Drain at top of `next()` |
| F-22 | P2 | PHP ext | Array/Table/Decimal AMQP headers silently dropped (`x-death` invisible to PHP) | CERTAIN | Wrong metadata | Encode or expose |
| F-23 | P2 | Core/ops | Recovery-generation failures only `eprintln!`; permanent topology errors loop invisibly | CERTAIN | Undiagnosable outage | Metric + classify 404/406 permanent |
| F-24 | P2 | Laravel/ops | `rabbit-rs:status` always reports zeros (fresh process, per-process metrics); documented duplicates-monitoring flow is void | CERTAIN | False confidence | In-process exporter; fix doc |
| F-25 | P2 | CI | `coverage.yml` runs Pest with `|| true` — red tests, green job | CERTAIN | Coverage theater | Remove `|| true` |
| F-26 | P2 | Release | Release published before PIE verify / mirror tag / Packagist verify (design order inverted) | CERTAIN | Broken live releases | Reorder DAG |
| F-27 | P2 | Laravel/boot | Strict env-bool validation in `register()`/`boot()` crashes whole app at boot for a driver typo | PROBABLE | Blast radius | Accept env strings / lazy normalize |
| F-28 | P2 | Laravel/Octane | Connector closure freezes normalized config; `octane:reload` never re-normalizes | PROBABLE | Stale brokers/creds | Lazy normalization by fingerprint |
| F-29 | P2 | Perf | Single tokio worker thread for all pools/actors (default) | CERTAIN | Throughput ceiling ~30–60k msg/s | `available_parallelism().min(4)` |
| F-30 | P2 | Perf | Confirm hot path: per-message timer + ~10 allocs + 4 hash ops; consume: 1 FFI crossing + 2–3 wakeups/delivery; ack: individual AMQP frames on quorum queues (batch API unused) | CERTAIN | 2–4 µs/msg + broker ack cost | Shared deadline timer; `nextBatch`/`ackThrough` in worker loop |
| F-31 | P2 | Core/config | `publisher.mandatory` flag ignored (`mandatory_flag = safety==Safe`); docs present it as working; fingerprint splits pools on dead value | CERTAIN | Behavior change on upgrade | Honor or reject flag |
| F-32 | P2 | Core/config | `confirm_timeout: 0` and `heartbeat: 0` accepted → instant expiry / dead-peer detection off | CERTAIN | Silent guarantee loss | Validate ≥ 1 s |
| F-33 | P2 | Tests | Stale-ACK guard, byte budgets, `map_lapin_error` untested; fire-and-forget Pest tests assert nothing; FPM test local-only; tautology + real sleep + no-op test | CERTAIN | Mutation passes | Targeted tests |
| F-34 | P2 | Tests | 4 PHPT scenario suites are untracked local files, never run | CERTAIN | False coverage | Commit & wire or delete |
| F-35 | P2 | Distribution | Design doc claims ZTS + 16 artifacts; composer says `support-zts: false`, matrix ships 10 NTS-only | CERTAIN | Contract confusion | Amend design doc (like max_in_flight note) |
| F-36 | P2 | Core/topology | Delay-queue args not in queue name + no GC: rolling config change → 406 storms; orphan queues forever | CERTAIN (mechanism) | Deploy outage; broker cruft | Hash args into name; GC |
| F-37 | P3 | Ops/CI | Dependabot composer entry dead (no lockfile); `RABBIT_RS_WORKER` env key dead; Horizon missing → fatal `Error`; coverage-laravel builds ext but never loads it; llvm tools from PATH vs rustup; `test-integration.sh` leaks Docker lab on failure; `symfony/process` not required; `^0.0` hard-coded; bash-4-only `${VERSION,,}`; dead `WorkerProfileResolver`; `check.sh` lacks `cargo deny`; nextest `retries=1` masks flakes | CERTAIN | assorted | see §5 table in repo |
| F-38 | P3 | Perf | Dead `exchange`/`routing_key` copies per delivery; scheduler O(n²) `contains` + alloc per delivery; `next_deadline` O(n) scan per event while suspended; properties cloned twice per publish; `flush_batch` deep clones; `early_ack` spawns a task per message; 1 ms flush interval → per-message `block_on` at low rates; `EventBridge::drain` overhead without callbacks | CERTAIN | sub-µs to µs each | see details in agent report §Performance |
| F-39 | P4 | Docs | Drift: "duplicates measurable" without redelivery metric; queue-expiry formula; `confirmations_total` ACK-only doc; "6 files"; stale plan status; `Pool::stats()` stub missing 12 keys; Horizon queue attribution wrong for multi-sub profiles; recovery order doc says replay last, code replays before consumers | CERTAIN | Confusion | Update docs |
| F-40 | P4 | Over-eng | Dead knobs/dead code: `publisher.mandatory` (31), `PublishOutcome::Ambiguous` never constructed, `max_in_flight` accept-and-ignore, `EXIT_SIGNAL` const, `WorkerProfileResolver` | CERTAIN | ~-60 lines | delete |

No P0 besides F-01. No fabricated findings: every row was re-verified against source by the lead auditor; two agents independently converged on F-01.

---

## 3. Detailed findings

### [P0] F-01 — No production connection-loss detection; recovery never triggers

**Localisation:** `crates/rabbit-rs-core/src/pool/connection_actor.rs:303-343`; `crates/rabbit-rs-core/src/consumer/set.rs:245-264`; `crates/rabbit-rs-core/src/transport/lapin.rs:36-37`; `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:277,373`

**Confiance:** CERTAIN (control flow verified by two independent passes + lead; lapin default verified in vendored lapin 4.10.0: `auto_recover: false`).

**Constat:** The connection actor in `Ready` phase blocks on `context.commands.recv().await` only — no select over any transport liveness source. `Command::ConnectionLost` is sent from exactly two production sites, both *internal recovery failures* (consumer establishment, generation recovery), plus a `#[cfg(test)]` helper. The `Transport`/`TransportConnection` traits expose no error/liveness stream. Lapin is built with `ConnectionProperties::default()` and its `auto_recover` defaults to `false` — and this codebase implements the recovery itself, so nothing monitors the socket. On the consumer side, `spawn_source` runs `while let Some(result) = stream.next().await` and returns silently when the stream ends; the actor keeps running, `buffer_tx` stays open, and `ConsumerHandle::next()` parks forever on an empty channel.

**Pourquoi:** Recovery was designed as event-driven with the implicit assumption "someone reports the loss". Nobody does. All recovery tests inject `connection_lost` by hand.

**Scénario de défaillance:** RabbitMQ restarts for maintenance at 03:00. FPM pods: first publish fails → publisher actor self-suspends; no `Ready` event ever arrives (F-02/F-03 family) → every subsequent publish replays until its 30 s deadline, then fails `Timeout` — forever, in every PHP process. `queue:work` children: the cached consumer's delivery stream is dead → `next()` parks forever with no error. `reconnects_total` stays 0; the states map still says `ready`; no callback fires. Monitoring shows a healthy system.

**Impact:** Availability (total, silent, per-process until restart), and the core product promise is unimplemented. At-least-once itself is *not* violated (broker redelivers; replay preserves `message_id`/deadline until expiry), but the availability contract is destroyed.

**Comment le vérifier:** Integration test against the lab: connect, kill the broker, publish → assert recovery occurs without any explicit `connection_lost` call. Today both publishers and consumers wedge.

**Correction:** Extend the `Transport` abstraction with a liveness surface (lapin `on_error`/heartbeat failure stream), select over it in the `Ready` loop and route to `connection_lost`; additionally treat consumer-stream termination as a loss trigger instead of returning silently.

---

### [P1] F-02 — `delay.mode=auto` hard-codes the plugin strategy; a channel-level declare failure suspends the publisher forever

**Localisation:** `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:637` (`Plugin | Auto => DelayStrategy::Plugin`); `crates/rabbit-rs-core/src/publisher/actor.rs:986-994` (`Err(e) if e.is_recoverable() => return DelayTopologyOutcome::Suspend`); `crates/rabbit-rs-core/src/transport/lapin.rs:645-663` (every `ProtocolError` — including 540/406/404 — is "recoverable").

**Confiance:** CERTAIN.

**Constat:** `auto` (the documented default) compiles to the plugin strategy. The first delayed publish declares the `*.delayed` exchange; if the plugin is absent (not bundled with RabbitMQ), the 540 error is classified recoverable → `Suspend`. Publisher suspension (verified: `channel = None`, `Phase::Suspended`) is only exited by `PublisherConnectionEvent::Ready`, which the coordinator emits only on a *new connection generation* — and the connection is healthy.

**Pourquoi:** Single wake-up path for suspension is connection recovery; channel-level failures never generate a generation.

**Scénario:** Default config, broker without the plugin, one `later(60)` job → every subsequent publish on that broker (delayed or not) buffers and fails `Timeout` after `confirm_timeout`, for the life of the process. Recreating the `Pool` doesn't help (process-global lazy registry).

**Impact:** Total publish outage per PHP process, triggered by one delayed job, no error surfaced.

**Vérifier:** Lab broker without the plugin, `auto` mode, publish delayed then normal; observe timeouts and stuck utilization.

**Correction:** Probe plugin availability once in `auto` (or catch the declare failure and fall back to TTL buckets); never `Suspend` for a single message's topology failure — fail that message terminally.

---

### [P1] F-03 — Coordinator swallows publisher `Ready` event failures; failed `enable_confirms` bricks the publisher with its generation consumed

**Localisation:** `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:449-457` (`let _ = pub_handle.connection_event(...)`); `crates/rabbit-rs-core/src/publisher/actor.rs:630-644`.

**Confiance:** swallow CERTAIN; trigger PROBABLE (transient `confirm.select` failure during recovery).

**Constat:** If `enable_confirms()` fails on the fresh channel, the actor returns early — stays `Suspended`, `channel: None` — while recovery reports success and the generation is consumed. Nothing retries the publisher step.

**Impact:** Publisher outage on a "healthy" connection; no metric, no error.

**Correction:** Propagate the `connection_event` error from `recover_generation` (drop the `let _ =`) so the generation rolls back and recovery re-runs.

---

### [P1] F-04 — Unbounded `pending_incoming` in `no_ack` mode; the documented guard is never validated

**Localisation:** `crates/rabbit-rs-core/src/consumer/actor.rs:104,417-425`; `crates/rabbit-rs-core/src/config.rs:170-173,477-523`.

**Confiance:** CERTAIN.

**Constat:** Deliveries over `max_buffered_bytes` are pushed to the unbounded `pending_incoming` deque. In `no_ack` mode the broker auto-acks, so QoS prefetch never bounds delivery; the documented requirement "`no_ack` requires `early_ack` and `best_effort`" (`config.rs:170-173`) is enforced nowhere (`validate()` checks weights/prefetch only).

**Scénario:** `no_ack: true` consumer pauses on a million-message queue → multi-GB RSS in one process; FPM kills workers repeatedly.

**Correction:** Enforce `no_ack ⇒ early_ack` at validation, or bound `pending_incoming` by count and surface backpressure.

---

### [P1] F-05 — Dead-letter binding is declared only for the first subscription sharing a DLQ

**Localisation:** `crates/rabbit-rs-core/src/topology/plan.rs:206-214`; `crates/rabbit-rs-core/src/client.rs:733` (`routing_key = dl.routing_key ?? sub.queue`).

**Confiance:** CERTAIN (conditional on the config shape: dead_letter enabled, ≥2 subscriptions, no explicit routing_key — which is nullable and the Laravel normalizer permits).

**Constat:** The `seen_dlqs` dedup conflates "queue already declared" with "binding already declared". With two subscriptions and default per-source routing keys, only the first (dlq, routing_key) binding exists; the second subscription's dead-letters route to the DLX with an unbound key and are **silently dropped** (DLX republish is not mandatory).

**Impact:** Silent loss of poison messages exactly on the path meant to preserve them.

**Correction:** Emit one binding per distinct (dlq, routing_key) pair.

---

### [P1] F-06 — Delay exceeding the largest TTL bucket is delivered immediately (publish) or hot-loops (delayed release)

**Localisation:** `crates/rabbit-rs-core/src/publisher/actor.rs:967-973` (`let Ok(route) = ... else { return Ready }` — route error swallowed); `crates/rabbit-rs-core/src/publisher/delay.rs:37-40` (bucket exhaustion is the error); consumer mirror at `consumer/actor.rs:874-880`.

**Confiance:** CERTAIN.

**Constat:** TTL mode, `delay > max bucket` → route fails → publish proceeds to the **original exchange** with an `x-delay` header RabbitMQ ignores → immediate delivery. Nothing validates delays against buckets anywhere. The delayed-release path instead fails `Publish` without settling → redelivery → hot loop.

**Scénario:** `delay.mode=ttl`, default buckets (max 120 s), `$job->delay = 3600` → scheduled job executes now.

**Correction:** Validate `delay_ms` against `bucket_for` at the publish boundary (typed error), and settle capped delays terminally on the consumer side.

---

### [P1] F-07 — Attempts machinery silently corrupts retry accounting and can never terminate a poison message

**Localisation:** `crates/rabbit-rs-core/src/consumer/actor.rs:228-230` (`.unwrap_or(if redelivered { 2 } else { 1 })` — resolve error swallowed); `consumer/attempts.rs:19` (cap 20 hard-coded, not configurable); `consumer/actor.rs:874-880` (capped delayed release → `MaxAttempts` error, no settlement).

**Confiance:** CERTAIN.

**Constat:** Three coupled defects: (a) a message with attempts > 20 is delivered with fabricated `attempts = 2`; (b) Laravel `--tries>20` can never fail the job (attempts oscillate 2…20); (c) the 20th delayed release fails validation, leaves the delivery pending → redelivery → fails again forever, delay mechanism disabled.

**Impact:** Poison-message protection void; unbounded hot redelivery of the worst message.

**Correction:** Make `max_attempts` configurable (Laravel config → native), map `MaxAttempts` to a terminal settlement (`reject(requeue=false)` → DLX), never substitute a lower attempts value.

---

### [P1] F-08 — A recovered broker never rejoins a long-lived worker: one-shot `SourceReplaced` + PHP consumer cache never evicted

**Localisation:** `crates/rabbit-rs-core/src/consumer/composite.rs:357-377`; `packages/laravel-queue/src/RabbitMqQueue.php:377` (`$this->consumers[$profile] ??= ...`, no eviction in the catch at `:379-383`).

**Confiance:** CERTAIN.

**Constat:** The native side surfaces `SourceReplaced` exactly once ("re-fetch consumer"); the PHP side caches the retired composite indefinitely. Next `pop()` returns the same dead handle.

**Scénario:** Single-broker daemon worker; broker restart → worker stops consuming forever (other workers drain; single-worker setups stall), or hot exception loop.

**Correction:** `unset($this->consumers[$profile])` on `SourceReplaced`/`Closed` before rethrowing.

---

### [P1] F-09 — Supervisor counts clean exits as failures: `--max-jobs` shuts the whole pool down

**Localisation:** `packages/laravel-queue/src/Console/WorkerSupervisor.php:202-231` (no exit-code inspection; every non-running child schedules a backoff restart and burns restart budget).

**Confiance:** CERTAIN.

**Scénario:** `rabbit-rs:work --max-jobs=1000 --max-restarts=3` → each clean batch exit burns budget → after 4 cycles the supervisor stops **all** workers with `EXIT_MAX_RESTARTS`.

**Correction:** Reset restart budget on exit 0 / healthy runtime; no backoff for planned recycling.

---

### [P1] F-10 — CI never runs a real-broker test despite starting the RabbitMQ lab

**Localisation:** `.github/workflows/ci.yml:269-270` (`cargo nextest run -p rabbit-rs-core --test '*'` — no `--features integration`; feature is non-default, `crates/rabbit-rs-core/Cargo.toml:11`); `scripts/test-integration.sh` (Laravel `Integration` suite incl. `AtLeastOnceChaosTest.php`) is invoked by no workflow.

**Confiance:** CERTAIN.

**Impact:** Swap the AMQP scheme in `connection_uri`, drop the `x-delay` header, break stale-ACK rejection — CI stays green. The repo's primary property is validated only by a mock and by developers who remember to run a script.

**Correction:** Add `--features integration` to the CI nextest invocation and run `./scripts/test-integration.sh` in the lab job; remove `|| true` from `coverage.yml:105` (F-25).

---

### [P1] F-11 — Poison payload throws before any settlement: unkillable message, worker stall

**Localisation:** `packages/laravel-queue/src/Jobs/RabbitMqJob.php:38-44` (throws in `marshalJob`); `RabbitMqQueue::pop` never settles.

**Confiance:** CERTAIN.

**Constat:** Non-JSON payload → constructor throws → delivery never acked/rejected → redelivered forever, prefetch slot burned; several such messages stall the worker. No DLQ escape (the connector itself warns the default has no `delivery_limit`).

**Correction:** Reject(requeue=false) unmarshable payloads (or ack-and-log after N attempts) instead of leaving them pending.

---

### P2 findings (condensed)

- **F-12** `recovery_coordinator.rs:189` — `.expect("coordinator actor is alive")` on a watch that legitimately dies during `pool.close()`; panic crosses `block_on` at the FFI boundary. Return `Option`, map to `ClientError::closed()` (the adjacent `wait_for_transition` already does this correctly).
- **F-13** `client.rs:752-780` — `queue_size`/`purge_queue` cache a raw connection outside the coordinator, no staleness/recovery: after a broker restart these ops fail forever; also a second AMQP connection per vhost (violates the documented model).
- **F-14** `publisher/actor.rs:214-228` — blind mode bypasses `byte_budget` and capacity permits: the documented 64 MiB bound is unenforced (count-only), and `publisher_utilization` reports `(0, capacity)`.
- **F-15** `client.rs:162-163` — `publish_batch` doc promises "resolve every accepted publication" but `?` on a later `publisher()` discards earlier waiters: outcome ambiguity for safe-mode callers.
- **F-16** `consumer/set.rs:298-306` — `Drop` closes via `try_send` on a possibly-full channel: lost Close → actor + broker subscriptions + buffers leak.
- **F-17** `classes/bridge.rs:113,140` — `let _ = callback.invoke_unlocked(...)`: exceptions thrown inside user callbacks are consumed from `EG(exception)` and destroyed (verified against ext-php-rs 0.15.15: `take_exception()` moves the object; `Error::Exception` → stringified). Plus single-slot `CallbackSlot`: a second Laravel connection with the same native fingerprint silently steals both callbacks.
- **F-18** `classes/pool.rs:283-309` + `publish_buffer.rs:124-138` — `close()`/`__destruct` flush errors swallowed; deadline-expired re-buffered publications silently filtered out (contract says "no silent loss"); destruct blocks up to the caller's `timeout_ms` (default 30 s, max 24 h) — FPM `request_terminate_timeout` then kills the worker mid-shutdown.
- **F-19** `conversion.rs:91,168` — `let validate_keys = cfg!(debug_assertions)`: release builds accept unknown publish keys; a `delay_ms` typo publishes immediately. Trust-boundary validation must not depend on build profile.
- **F-20** `classes/consumer.rs:164-167` — `ackBatch` enforces the 256 cap mid-loop: 256 settlements already enqueued when it throws.
- **F-21** `classes/consumer.rs:48-55` — fast-path `next()` skips `bridge.drain()`: under steady traffic, connection-state/backpressure callbacks never fire.
- **F-22** `classes/delivery.rs:149` + `transport/lapin.rs:625` — AMQP `Array`/`Table`/`Decimal` headers silently dropped at the PHP boundary (`x-death` invisible).
- **F-23** `recovery_coordinator.rs:367` — generation-recovery failures are `eprintln!`-only; in FPM stderr is typically discarded; no metric; permanent topology errors loop invisibly.
- **F-24** `RabbitMqStatusCommand.php:44-62` — creates a fresh pool in the CLI process; per-process metrics mean the documented "monitor duplicates with `rabbit-rs:status`" flow always shows zeros.
- **F-25** `coverage.yml:105` — Pest under `|| true`: broken tests still produce a green coverage job (CI and local disagree).
- **F-26** `release.yml:286,336-346,392` — `publish-release` precedes `verify-pie-install` and the Laravel mirror tag, inverting the documented release gates; a failed PIE verification leaves a live, unproven release.
- **F-27** `RabbitMqServiceProvider.php:23-27,56-57` + `ConfigNormalizer.php:592-599` — normalization (strict throws) runs at `register()`/`boot()` for every request; `env('X')` returning `'1'` for booleans crashes the whole app at boot.
- **F-28** `RabbitMqServiceProvider.php:63-77` + `OctaneLifecycle.php:31-35` — connector closure freezes the normalized config; `octane:reload` flushes pools but the closure/singleton keep the stale brokers → credential rotation requires full worker restart.
- **F-29** `runtime.rs:45-51` — default runtime is **one tokio worker thread** for every pool/vhost/actor/IO loop in the process: all Rust-side per-message cost is serialized; saturates near 30–60k msg/s with head-of-line latency regardless of FPM worker count.
- **F-30** Confirm hot path (`publisher/actor.rs:589,709-763`): per-message `Box`/oneshot/single-VecDeque/HashMap inserts + a per-message `Sleep` in the timer wheel + individual `receipt.wait()` per confirm; consume (`set.rs:456-469`, `RabbitMqQueue.php:378`): one FFI crossing + 2–3 actor wakeups per delivery while `nextBatch` exists unused; ack (`RabbitMqJob.php:76`): individual `basic.ack` frames while `ackThrough`/`ackBatch` exist unused — on quorum queues each ack is Raft-replicated broker-side. Estimated: 2–4 µs/msg client-side + broker ack cost; the largest levers are API usage, not rewrites.
- **F-31** `publisher/mod.rs:215-217` + `config.rs:318-321` — `mandatory` deserialized, fingerprinted, documented, deprecated… and ignored (`mandatory_flag = safety==Safe`): behavior change on upgrade for configs relying on it.
- **F-32** `config.rs:414-549` — `confirm_timeout: 0` → instant deadline expiry for every publish; `heartbeat: 0` accepted → dead-peer detection off (compounds F-01).
- **F-33** Tests: stale-ACK guard has zero coverage (the test named `..._rejects_stale_acks` asserts generation eviction only); byte budgets never tripped by any fixture; `map_lapin_error` untested; `FireAndForgetTest` assertions vacuous; FPM fork test local-only; `tests/publisher.rs:1487` asserts `*i == *i`; `tests/consumer.rs:484` real 500 ms sleep (violates the repo's own paused-time rule); `consumer.rs:1424` no-op test.
- **F-34** `scripts/test-extension.sh:101-113` — only `extension_metadata.phpt` runs; the four scenario PHPT files (`backpressure`, `boundary_limits`, `delivery_terminal_state`, `publisher_outcomes`) are untracked local residue.
- **F-35** `composer.json:13` (`support-zts: false`) vs design doc §Distribution (ZTS, 16 artifacts) vs `release.yml` (`ts: ["nts"]`, 10 artifacts): pick a story and update the design doc with a dated note (precedent: the `max_in_flight` note).
- **F-36** `topology/delay.rs` — delay-queue name doesn't include its args (`x-message-ttl`, `x-expires`): two app versions with different margins declare the same queue name with different args → concurrent `PRECONDITION_FAILED` during rolling deploys; no GC ever deletes `rabbit-rs.delay.*` queues (bucket-list changes orphan them forever).
- **F-37** Ops/CI cluster: dependabot `composer` entry dead (no `composer.lock` anywhere, CI deletes it); `RABBIT_RS_WORKER` env key dead (connector reads only `queue.php`); `worker=horizon` without the package → fatal `Error`, not an actionable exception; `coverage-laravel.sh` builds the extension then never loads it (dead `ARTIFACT` var, faithfully reproduced in CI); `coverage-php-ext.sh` resolves `llvm-profdata`/`llvm-cov` from PATH instead of the rustup toolchain (contradicts AGENTS.md); `test-integration.sh` leaks the Docker lab on any failure (no `trap ... EXIT`); `symfony/process` used but only transitively required; `MissingExtensionException` hard-codes `^0.0`; `${VERSION,,}` breaks macOS bash 3.2; `WorkerProfileResolver` dead; `check.sh` doesn't run `cargo deny`; nextest default `retries=1` masks flaky async tests in the exact codebase where timing races are the core risk.
- **F-38** Minor perf: dead `exchange`/`routing_key` String allocs per delivery (`transport/lapin.rs:290-291`); scheduler O(n²) `contains` + heap alloc per dispatch, no n==1 fast path; `next_deadline` O(n) replay scan per select iteration while suspended; properties deep-cloned twice per publish (actor → TransportRequest → lapin), headers too; `flush_batch` deep-clones every buffered request; `early_ack` spawns a task per message; 1 ms publish-buffer flush interval → per-message `block_on` below ~1k msg/s; `EventBridge::drain()` map-clone + metrics snapshot per publish even with zero callbacks registered. All CERTAIN, each sub-µs to µs — do the zero-risk ones (dead copies, drain gating, n==1 short-circuit), skip the rest until profiled.
- **F-39** Docs drift: reliability.md promises measurable duplicates but `MetricsSnapshot` has no redelivery counter; topology.md queue-expiry formula wrong; `confirmations_total` documented "(ACK)" but counts Nacks too; development.md "6 test files" vs 9; implementation plan header stale ("Next step: Milestone F" vs "all complete"); `Pool::stats()` stub missing 12 keys (`deliveries_total`, acks, rejects, 6 latency percentiles); Horizon `JobReserved` attributes multi-subscription profiles to the pop-argument queue; recovery order documented replay-last but code replays before consumers.
- **F-40** Over-engineering (small, already mostly addressed by PR #65): dead `publisher.mandatory` knob, `PublishOutcome::Ambiguous` never constructed anywhere, `max_in_flight` accept-and-ignore field, `EXIT_SIGNAL` const, `WorkerProfileResolver`. `net: ~-60 lines`. Dependencies are lean — no cuts.

---

## 4. Root causes (symptoms traced to sources)

1. **Liveness is event-driven with no event producer.** The `Transport` abstraction has no error/liveness surface, and the recovery design assumes `connection_lost` arrives from somewhere. F-01, F-02, F-03, F-08, F-13, F-23 are all facets of this single gap. Fix the abstraction once (`error_stream()` + `Ready`-loop select) and half the P1 table collapses.
2. **"Refuse" is not "settle".** Every permanent refusal (max attempts, unmarshable payload, unroutable delay) leaves the work pending instead of driving a terminal settlement (`reject(requeue=false)` → DLX). F-07, F-11, F-06 (consumer side). One policy ("permanent failure ⇒ terminal settlement or explicit DLQ") fixes the family.
3. **Configuration surface is not executable.** Flags are parsed, fingerprinted, documented — and decoupled from behavior (`mandatory` ignored, `no_ack` guard unvalidated, `confirm_timeout: 0` accepted, dead env key, debug-only validation). There is no test that pins *documented guarantees to code paths*. The docs are better than the enforcement.
4. **CI validates the mock, not the product.** The mock transport is excellent, and it is also the only thing CI exercises against the broker layer. Every seam (lapin adapter, extension↔broker, FPM/Octane) is CI-blind (`--features integration` missing, Laravel chaos suite unwired, PHPT suites unwired, FPM test local-only, coverage `|| true`).
5. **Observability ends where incidents begin.** Metrics exist and are lock-free, but recovery failures, dropped publications, redeliveries, and cross-process views are absent (`eprintln!`, silent filters, per-process zeros). The 3am story for any of the P0/P1 findings is: nothing in the dashboards moves.

---

## 5. Top 10 actions (impact / effort)

| # | Action | Fixes | Benefit | Effort | Risk of not doing | Dependencies |
|---|--------|-------|---------|--------|-------------------|--------------|
| 1 | Transport liveness: `error_stream()` on `TransportConnection`, select in `Ready` loop → `connection_lost`; consumer stream end → terminal error | F-01 | Product promise actually works | Moyen | Every broker restart = silent fleet outage | transport.rs API bump |
| 2 | Publisher wake-up fixes: propagate `Ready` errors (drop `let _ =`), never `Suspend` on single-message declare failure, probe plugin in `auto` | F-02, F-03 | End of the brick family | Faible | One delayed job kills publishing | — |
| 3 | CI: `--features integration` in nextest, run `test-integration.sh` in the lab job, remove `\|\| true`, wire the 4 PHPT files | F-10, F-25, F-34 | Regressions at the seams become visible | Faible | Every future fix unverified against reality | lab in CI (already started) |
| 4 | Enforce `no_ack ⇒ early_ack` (or bound `pending_incoming`) | F-04 | OOM eliminated | Faible | Process kills under load | — |
| 5 | Binding per (dlq, routing_key) pair | F-05 | No silent poison loss | Faible | Contract violation on the DLQ path | — |
| 6 | Attempts: configurable cap, terminal settle on cap, no fabricated attempts | F-07 | Poison loops end; Laravel `--tries` restored | Moyen | Hot redelivery storms | F-01 (DLQ needs a live channel) |
| 7 | PHP: evict consumer cache on `SourceReplaced`/`Closed`; reject unmarshable payloads terminally | F-08, F-11 | Workers survive recoveries | Faible | Silent per-worker stop after every recovery | — |
| 8 | Supervisor: reset restart budget on clean exit | F-09 | `--max-jobs` usable | Faible | Self-inflicted fleet stop | — |
| 9 | Delay boundary validation (publish + release) | F-06 | No wrong-time execution | Faible | Silent semantic inversion | — |
| 10 | Runtime `worker_threads` default + deadline-sweep timer instead of per-message `timeout_at` | F-29, F-30 | ~2–4 µs/msg and multi-core headroom | Faible/Moyen | Ceiling at 10× traffic | benchmark before/after |

---

## 6. The 3 things I would do immediately

1. **Implement transport liveness detection and route it to `connection_lost`** (action 1). Without it, the product's defining feature — automatic recovery — is dead code in production, and every other reliability property is unobservable.
2. **Ship the two small publisher fixes** (action 2): drop `let _ =` on the `Ready` event, and stop suspending the publisher for a single message's declare failure (probe the plugin in `auto`). Both are small diffs that remove two permanent-outage classes.
3. **Make CI run the real thing** (action 3): the lab is already booted; one missing cargo flag and one missing script call separate you from validating the delivery contract where it actually lives. This converts every other fix on this list from "believed" to "proven".

---

## 7. Method, verification status, and what could not be concluded

- **Verification:** every finding above was re-read at the cited location by the lead auditor; the two agents that analyzed the core independently converged on F-01 via different paths. Local run: `cargo nextest run --workspace --all-targets` → 275 passed (default features — which is itself F-10's point).
- **Could not be concluded (and what's missing):** exact lapin stream-termination latency on socket death (needs a live-broker kill test — the fix for F-01 should come with one); real-world throughput numbers (needs the benchmark harness on the reference machine); ZTS soundness claims (needs a ZTS build in CI before the design doc claim is restored); `cargo deny` advisory freshness (CI-only at audit time); Octane reload semantics across all four servers (only generic Laravel/Octane behavior was verifiable).
- **Not audited:** `benchmarks/results/` data, Homebrew formula runtime behavior, PIE binary end-to-end install on the 16-artifact matrix (only workflow logic reviewed).

---

## 8. Tracking — findings to issues (Round G, 2026-08-31)

Findings not already covered by existing tracker issues were split into Round G (Tasks 21–38, #66–#83), organized in 7 file-disjoint parallel tracks; the epic (#62) and `docs/plans/ROADMAP.md` hold the execution order. Findings already tracked at audit time:

| Finding | Tracked by |
|---------|------------|
| F-01 (P0), F-23 (recovery counter part) | #66 (Task 21) |
| F-02, F-03 | #67 (Task 22) |
| F-04, F-16 | #71 (Task 26) |
| F-05 | #72 (Task 27) |
| F-06 | #73 (Task 28) + consumer side in #70 (Task 25) |
| F-07, F-11 | #70 (Task 25) |
| F-08 | #68 (Task 23) |
| F-09 | #74 (Task 29) — follow-up to #59 |
| F-10, F-25, F-34 | #69 (Task 24) — pairs with #40 |
| F-12 | #56 (comment added) |
| F-13 | #77 (Task 32) |
| F-14 | #52 (pre-existing, exact match) |
| F-15 | #83 (Task 38) |
| F-17, F-21 | #75 (Task 30) |
| F-18, F-19, F-20, F-22 | #76 (Task 31) |
| F-24 | #81 (Task 36) — after #50 |
| F-26 | Parked in ROADMAP (documented PIE trade-off) |
| F-27, F-28 | #80 (Task 35) |
| F-29, F-30 | #41 (comment added — perf leads for Round D) |
| F-31, F-32 | #78 (Task 33) |
| F-33 | test gaps folded into #66/#71/#70; leftovers in #82 (Task 37) |
| F-35, F-39 (drift items) | #60 (comment added) |
| F-36 | #79 (Task 34) |
| F-37, F-40 | #82 (Task 37) |
| F-38 | Parked in ROADMAP + #41 comment |
| C13 metrics drift | recovery counter in #66; remainder observability polish (#81/#82) |
