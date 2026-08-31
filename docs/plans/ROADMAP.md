# Roadmap

Running trace of upcoming work. When a round starts it gets a full dated plan in
`docs/plans/`; this file keeps the queue, motivation, scope, and success criteria so
nothing is lost between rounds.

## Landed

- Native design + implementation plans (2026-07-30) — milestone details live there.
- Fair benchmark comparison (2026-08-16) and benchmark fixes (2026-08-21).
- Publisher batcher removal (2026-08-21) and publish pump v2 (2026-08-28).
- Post-pump simplification (2026-08-29, plan `2026-08-29-post-pump-simplification.md`):
  Phase A realistic Laravel benchmarks, Phase B dead-code sweep, Phase C mimalloc
  (evaluated, rejected on data), Phase D publisher.safety plumbing + invariant tests +
  multi-broker consumer fan-in, Phase E driver-level benchmark vs amqplib/amqp-ext
  (merged as PR #35, 2026-08-30 — includes the local ext-amqp cross-check and the
  publish safety variant matrix).

## Next — Round 2: consumer stall and reliability

Full plan: `docs/plans/2026-08-30-consumer-stall-and-reliability.md`.

Found by the Phase E driver-level benchmark; all three defects reproduce on demand and
none is a delivery loss (messages stay `ready` in RabbitMQ; at-least-once holds).

1. **P1 — stall of the ack pipeline under sustained pop+ack.** The consumer stops
   receiving while messages stay ready (observed with prefetch 1 and 64). The bench
   runner detects it (400 consecutive null pops) and rebuilds the connection;
   ~0.6–0.8 s per stall is billed into measured time (`stall_recoveries` reported).
   First task: root-cause investigation (prior exploration came back empty).
   **First hypothesis to test (from the 2026-08-30 production-readiness audit):** the
   per-broker consumer actor blocks on a *blocking* send to the bounded
   settlement-error channel — 11 `state.error_tx.send(...)` sites in
   `crates/rabbit-rs-core/src/consumer/actor.rs` on a `flume::bounded(256)` channel
   (`consumer/set.rs`). If the embedder never calls `drain_errors()`, 256 settlement
   errors freeze the actor loop: deliveries stop flowing while messages stay `ready`
   — the exact observed symptom. The fix (drop-oldest wrapper, ~30 lines) is
   Round F Task 1; apply it before deeper root-cause work and re-run the Phase E
   reproduction to confirm or eliminate it.
2. **P2 — pre-fill missing deliveries (~2%).** If the native consumer is created
   before the fill has been ingested (first `pop()` while the fill is in flight, or a
   consumer left idle across rounds), a fraction of messages never surfaces on that
   connection. Verified: consumer created pre-fill → ~2% missed; created after the
   fill → clean. Other drivers (amqplib, amqp-ext, bunny) are unaffected.
3. **P3 — `Pool::clear()` with a pre-existing consumer.** Combo degrades pops ~25×.
   Needs a dedicated core-level test.
   **Resolved 2026-08-30 (no core defect):** the dedicated core tests
   (`crates/rabbit-rs-core/tests/pool_clear.rs`) show the combination is safe —
   deliveries keep flowing, generations and settlements are untouched, and no
   re-establishment or connection storm exists. The observed ~25× is the P2
   configuration (consumer attached while the next round's fill is ingested,
   plus the 404→pop-drain fallback) amplified by the matrix's 1 s null-pop
   block; see the P3 section of the round plan for the full mechanism and the
   documented `clear()` → consumer sequence.

Secondary scope:

4. **Test the closed-pump batch contract** (`client.rs:143-147`): batch must fail
   immediately and re-buffer (superset semantics) — ~10 lines, parked since Phase B.
5. **`scripts/lib-extension.sh` rebuild-on-change**: currently builds only when the
   artifact is missing; stale artifacts after `Cargo.toml`/lock changes remain a
   footgun (the D2 fix covers missing + warning only).
6. **Parked minors rolled in**: symmetric `flush_blind` flush-vacue test
   (`blind_pump.rs`), shellcheck pass on `scripts/test-integration.sh`,
   subscription-name uniqueness validation.

Then: **re-bench** with the Phase E 100-run protocol to quantify the delta (worker
goopil was 10 030 ops/s median with the stall tax vs 27 073 on clean rounds; the fix
should close most of that).

Success criteria: each bug root-caused and fixed (or its ceiling documented with
data), full quality gate green, re-bench archived and compared.

## Round F — production readiness hardening (audit 2026-08-30)

Motivation: full-stack production-readiness audit of the ecosystem (core Rust, PHP
extension, Laravel package) on 2026-08-30. Detailed TDD plan with exact files, test
code and verification commands:
`docs/superpowers/plans/2026-08-30-production-readiness.md` (tasks 1–20). Split into
two waves around Round 2 and the re-bench.

### F1 — correctness & safety blockers (runs alongside Round 2)

1. **Consumer actor: make the settlement-error channel non-blocking** (drop-oldest,
   plan Task 1) — also Round 2 P1 hypothesis #1; fix first, then re-test the stall
   reproduction.
2. **Horizon: honor `after_commit` and wire `bulk()`** (plan Task 5) — the Horizon
   subclass bypasses `enqueueUsing`, so transactional jobs publish before the SQL
   commit (job loss on rollback); `bulk()` skips `JobPayload::prepare()` so bulk jobs
   are invisible in the dashboard. Prerequisite: base `prepareBatch`/`publishBatch`
   go from `private` to `protected`.
3. **Bound the extension publish buffer** (plan Task 2) — `publish_buffer`
   (`crates/rabbit-rs-php/src/classes/pool.rs:46`) grows unbounded during outages;
   cap at 4096 messages / 64 MiB with explicit `BackpressureException` (already
   accepted messages are never dropped). Includes a benchmark non-regression step.
4. **Consumer acquisition deadline** (plan Task 3) — `ClientPool::consumer()`
   (`client.rs:330+`) loops forever against a black-holed broker and freezes FPM
   workers; add `consumer.wait_timeout` (default 30 s, validated 1 s–24 h), typed
   transport error at expiry, Laravel `consumers.wait_timeout`.
5. **TLS: fail loudly on unreadable certificate files** (plan Task 4) —
   `fs::read(...).ok()` in `transport/lapin.rs` silently connects without the
   configured CA; return a typed error identifying the exact path.
6. **Poison-message warning on permissive defaults** (plan Task 6) —
   `delivery_limit=null` + `dead_letter=null` means infinite redelivery for
   worker-crashing messages; emit a production warning (opt-out
   `production_warning`).

### F2 — hardening (after Round 2 + re-bench)

7. **Lazy consumer establishment** (plan Task 7) — `recover_generation`
   (`recovery_coordinator.rs:406`) consumes every declared worker profile, so a
   publisher-only process retains unacked messages on all queues at each
   reconnection; only establish requested profiles.
8. **Wire duplicate measurement** (plan Task 9) — `record_duplicate()` has no
   callers; the at-least-once contract requires duplicates to be measurable.
9. **Drain native events on publish/consume paths** (plan Task 10) —
   `ConnectionStateChanged`/`BackpressureDetected` only fire inside `stats()`
   (`pool.rs:264-265`); extract a shared `EventBridge` used by `Pool` and
   `Consumer` so the README/operations.md promises become true.
10. **Blind mode byte budget** (plan Task 11) — `publish_blind`
    (`publisher/actor.rs:229`) bypasses the byte budget (message-count only);
    reuse `with_byte_budget` (`publisher/mod.rs:245`). Includes a benchmark
    non-regression step.
11. **TLS integration: SNI + verify + lab TLS profile** (plan Task 12) —
    `TlsVerify::None` and `server_name` are validated but never read by the
    transport; API decision gated on lapin 4.10 connector capabilities, TLS
    profile in the lab, handshake/untrusted-CA/SNI tests.
12. **PIE end-to-end validation** (plan Task 13) — NTS naming inconsistency
    between `release.yml` and `package-pie-binary.sh`; no `pie install` test in
    CI. Unify naming, add a blocking CI job on a draft release. *Can start in
    parallel from F1 onward (external latency: release cycle).*
13. **Laravel contracts: `ClearableQueue` + optional `auto_subscribe`** (plan
    Task 14) — `queue:clear` fails (interface not declared) and `pop()` refuses
    plain queue names (deviation from the Laravel convention).

### F3 — ecosystem & DX (with F2)

15. **Log facade, typed coordinator errors, zero panics reachable from PHP**
    (plan Task 15) — `eprintln!`, `CoordinatorError = String`, `expect()`/panics
    in spawned tasks (process abort under FFI).
16. **Version alignment + CHANGELOG** (plan Task 16) — composer `^0.0` vs
    exception `^1.0` vs docs `1.0.0` vs workspace `0.0.7`.
17. **Install friction + static analysis** (plan Task 17) — `ext-rabbit_rs` as a
    hard `require` breaks `composer install` without the extension; move to
    `suggest` + runtime validation at connection resolution; add Pint + PHPStan
    to the quality gate.
18. **Harden the worker supervisor** (plan Task 18) — pcntl-free `--workers=1`
    path (the current error message is wrong) and non-blocking restart backoff.
19. **Docs/stubs alignment** (plan Task 19) — `stats()` stub documents 8 keys
    for 17 real ones; prefetch 16 advertised vs 64 configured; PHPUnit suite
    references that do not exist (Pest is used); nonexistent `dispatchBatch` API
    in docs.
20. **ZTS decision applied** (plan Task 20) — Option A: drop ZTS from the V1
    release matrix (16 → 8 assets), revisit in V2. See Parked.

Success criteria: all plan tasks green (tasks 1–7, 9–20), full quality gate, no
benchmark regression against the frozen budgets, `pie install` validated on a real
release.

## Round C — local integration harness reliability

Motivation: 8 Laravel integration tests fail on this machine for pre-existing,
environment-level reasons (protected `$app` chaos, multi-node ConnectionException,
TypeError whoops). CI is green, so this is local-only — but it blinds local
regression detection for every future round that touches consumer/connection code.

Scope: diagnose each failure class; fix the harness/environment (not product code);
either reach a locally green `./scripts/test-integration.sh` on a clean environment or
document explicit, honest skip criteria.

Success criteria: integration suite reproducibly green locally, or documented skip
criteria enforced by the harness.

## Round D — dispatch gap investigation

Motivation: the publish-side gap is now precisely localized. In fire-and-forget
rabbit-rs already leads (framework: 76 794 vs 31 182 ops/s vs vladimir, ~2.46×;
transport: ~2.8×) — nothing to optimize there. The whole remaining gap is the
**safe per-message confirm+mandatory path**: 0.31× vladimir at framework level
(9 772 vs 31 182 ops/s, same-session interleaved), 0.28× at transport level
(8 213 vs 29 648). goopil's own safety ladder prices it: blind 76.8k → unsafe
62.5k (×1.23) → safe 9.8k (×6.4) — the unsafe→safe step is the target.
Secondary target: batch-confirm at transport level, 0.68× vs bunny/amqplib.

Precondition: Round 2 landed and re-benched (clean consumer baseline).

Scope: profile the safe publish path end to end — flamegraphs plus per-stage
breakdown (PHP extension boundary, pump hand-off per message, confirm waiter,
mandatory-return handling, socket write) — then either implement targeted
optimizations (with re-bench proof) or document the ceiling with data.

Success criteria: root cause documented with measurements; either an implemented,
bench-validated optimization or a documented, quantified ceiling.

## Round E — adaptive prefetch per subscription

Motivation: fixed prefetch cannot serve opposite queue profiles at once — fast jobs
under-fill the pipeline (RTT visible, throughput capped at `prefetch / job duration`)
while slow jobs with high prefetch waste memory and amplify the post-crash redelivery
blast radius. Anticipated by the original design ("adaptive prefetch based on EWMA,
target buffer time, hysteresis").

Full design (approved spec):
`docs/superpowers/specs/2026-08-29-adaptive-prefetch-design.md`; implementation
plan: `docs/superpowers/plans/2026-08-29-adaptive-prefetch.md`.

Scope: per-subscription adaptive prefetch driven by an EWMA of settlement latency
(includes PHP job duration), target buffer time, 25% hysteresis, 1s tick in the
consumer actor with `set_qos` applied off the critical path; union wire format
(`fixed`/`adaptive`) backward compatible with the plain int form; adaptive rejected
with `early_ack`/`no_ack` at validation; buffer sizing from the sum of maxima;
`Consumer::getPrefetchStats()` observability; Laravel normalizer + docs.

Success criteria: full quality gate green; deterministic paused-time tests prove the
QoS adjustment sequence on the mock transport; fixed mode behavior byte-identical
(regression tests).

## Order of execution (agreed 2026-08-30)

Round 2 (starting with the error_tx drop-oldest fix as the P1 hypothesis) →
Round F1 → Round C (local harness, unblocks local validation of everything after) →
Round F2 → Round E → Round D. The PIE validation (Round F2 item 12) can start in
parallel from F1 onward because it only depends on a release cycle, not on code.

## Parked (no round yet)

- **Per-queue publish safety**: `publisher.safety` is a connection-level setting
  (ConfigNormalizer validates safe|unsafe|blind; the core applies one SafetyMode
  per connection/vhost). Several Laravel connections (same broker, distinct
  vhosts) already give per-vhost safety today. A true per-queue safety inside one
  connection would be a core config-surface extension — arbitrate alongside
  Round D.
- Publish latency excludes the terminal flush (comment if ever reported).
- Re-fetch acquisition blocks on all brokers when a source retires — semantics match
  mono-broker behavior and are documented; revisit only if an async retire is needed.
- **ZTS decision (APPLIED for V1, plan Task 20 — Option A)**: ZTS was
  unproven — the process-global `RuntimeRegistry` is shared across PHP threads,
  the CI ZTS job was advisory-only (`continue-on-error`) and release ZTS
  artifacts only got an `extension_loaded` smoke test. Applied for V1: ZTS
  dropped from the release matrix (16 → 8 assets, `support-zts: false` in the
  root `composer.json`) and the advisory ZTS CI job removed. Revisit in V2 with
  thread isolation (TSRM-aware registry) + a blocking ZTS CI job + real
  concurrency tests.
- **Known minor items from the audit, deliberately unscheduled**:
  `try_publish_hot` is a pure passthrough of `try_publish` (the announced hot path
  does not exist yet); `frame_max = 1 MiB` is hardcoded in `transport/lapin.rs`;
  AGENTS.md says 6 core test files but 8 exist (blind_pump, transport_tuning).

## Housekeeping (owner actions)

- ~~`git pull` the main checkout~~ (done 2026-08-30 — main checkout is current).
- Delete the orphan SonarCloud project `Goopil_rabbit-rs` and the now-unused
  `SONAR_TOKEN` secret (coverage goes to Codecov only; SonarCloud runs automatic
  analysis on `Goopil_php-rabbit-rs`).
