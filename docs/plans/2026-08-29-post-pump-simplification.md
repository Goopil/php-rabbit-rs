# Post-pump: Laravel benchmarks, sweep, mimalloc

Date: 2026-08-29
Base: main (d923fe1, PR #30 merged) — Phase A branch `bench/laravel-realistic` already created (empty, on d923fe1)
Previous: `2026-08-28-publish-pump-v2.md` (executed, merged)

## Context

Pump v2 is merged: 268k msg/s publish fire-and-forget in release (×2.7 the 80-100k
target), publish AA +44%, consume/p99 at parity, 0 losses/duplicates. This thread now
becomes consolidation:

1. Measure what is **actually used on the Laravel side** (current benchmarks measure
   `publishBatch` + 256 B payload, whereas Laravel publishes unitary buffered Safe
   with ~1-2 KB envelopes and consumes unitary + ack per job).
2. Simplify the codebase now that performance is there (dead code sweep).
3. Evaluate an alternative allocator (mimalloc) with honest measurements.

The scenarios created in Phase A then serve as the measurement baseline for B (sweep
confirmation) and C (mimalloc A/B) — hence the A → B → C order.

## Settled decisions (archived — do not reopen)

- **Pump-replay / mode collapse**: rejected. Unsafe-actor has unique qualities
  (per-message waiters, metrics + `BackpressureDetected`, clean close, delay routing
  — the pump bypasses `delay_ms > 0`). Blind-pump and Unsafe-actor are two distinct products.
- **Removal of the PHP publish buffer** (`publish_buffer` 64/1ms, pool.rs): rejected.
  It carries Safe mode (Laravel default): amortizes ack RTTs (64 msgs/RTT instead of
  ~1000 msg/s per unitary message). Laravel: `push/pushRaw/later/Horizon` →
  buffered `pool->publish()` (RabbitMqQueue.php:436); `bulk` → `publishBatch` (:292).
- **parking_lot**: rejected (<1-2%; hot locks rare and uncontended; metrics already
  lock-free atomics, metrics.rs:25).
- **Poisoning**: closed, nothing to do. 2 coherent styles (fail-fast pool.rs/
  callbacks.rs `.expect("... poisoned")`; tolerant consumer/set.rs ×4 + runtime.rs:139
  `unwrap_or_else(PoisonError::into_inner)`); `tokio::sync::Mutex` has no poisoning;
  critical sections are trivial and panic-free by design.

## Global constraints

- Full gate before declaring a task done: `rtk ./scripts/check.sh` +
  `./scripts/test-extension.sh` + `./scripts/test-laravel.sh`.
- Benchmarks in **release only** (debug masks ~4×); interleaved runs; keep per-run
  JSONs in the SDD workspace.
- Rebuild the extension with `--features extension-tests` after any Rust change
  (otherwise 38 phantom `testing_pool` failures); mandatory rebuild after a version bump.
- The at-least-once contract is untouchable for Safe/Unsafe. Blind = explicit
  fire-and-forget (silent loss on transport error, documented). Crash-safe = external
  outbox, out of scope.
- SDD for each phase (file brief → implementer → reviewer → ledger in
  `.superpowers/sdd/<date>-<slug>/`).
- Separate MRs, each from main, explicitly validated by the user before execution.

## Phase A — Laravel-representative benchmarks (MR 1) — MERGED (PR #32, 2026-08-29)

Executed in SDD (Tasks 1-4, ledger `.superpowers/sdd/2026-08-29-post-pump-simplification/`).
Key results (release, 5 scenarios × 3 drivers, 0 losses/0 dups): unitary-Safe publish
rabbit-rs ~3× SLOWER than the PHP drivers (9 755 vs 28 922-30 093 msg/s, high variance)
— main input for Phases B/C; rabbit-rs worker consume 5-6× faster (push vs pull, honest
mechanical gap); p99 worker drain-dominated = scenario definition artifact. Smoke budget
unchanged.

Branch: `bench/laravel-realistic` (created on d923fe1).

### Task 1 — Framework + scenarios + rabbit-rs driver

Verified facts: scenarios registered in run-benchmarks.php:60-64; drivers
auto-detected (:47-58, amqp-ext skipped if absent); drivers wired via `match` on
`$this->scenarioMode` (RabbitRsDriver.php:46-72); publish/consume phases timed
separately (fill-then-drain, AbstractBenchmark.php:51-92); global budget
smoke-budget.json (min 1000 pub / 500 consume, losses=0); Laravel default prefetch = 64
(packages/laravel-queue/config/rabbit-rs.php:208).

- `ScenarioMode.php`: + `LARAVEL_DISPATCH = 'laravel-dispatch'`,
  `LARAVEL_WORKER = 'laravel-worker'`.
- `Config.php`: + `MESSAGE_PAYLOAD_LARAVEL_BYTES = 1024` (Laravel envelope ~1-2 KB),
  + `PREFETCH_LARAVEL = 64` (Laravel default, vs current 128).
- `AbstractBenchmark.php`: overridable `protected int $payloadBytes` property,
  used by `createMessage()` (replaces direct constant access :30-44).
- Two scenarios with orthogonal headlines (the non-measured phase is a fill/drain as
  fast as possible to avoid polluting the signal):
  - `Scenarios/LaravelDispatchBenchmark.php` — headline **publish**: unitary publish
    (`pool->publish()`) ×10k in Safe (confirms+mandatory), 1024 B payload; fast batch
    drain.
  - `Scenarios/LaravelWorkerBenchmark.php` — headline **consume**: fast fill
    (publishBatch blind), unitary `next()` consume + ack per message, 1024 B payload,
    prefetch 64.
- `run-benchmarks.php`: + 2 entries in the `$scenarios` map.
- `RabbitRsDriver.php`: `match` branches for the 2 modes — dispatch: confirms/mandatory/safe
  config + unitary publish; worker: existing consume else-arm
  (tryNext/next + ack, RabbitRsDriver.php:159-183) with prefetch 64 in the config.
- Gate: `composer validate`, targeted smoke `--scenario=laravel-dispatch --driver=rabbit-rs`.

### Task 2 — Wiring the 3 other drivers

- amqplib: unitary `basic_publish` publish + `wait_for_pending_acks_returns` every
  64 (mirrors the rabbit-rs buffer flush → fairly amortized RTT); worker consume =
  basic_get + unitary ack.
- bunny / amqp-ext: same principles, according to their existing confirms support on
  the batch-confirm path (batch 256 → flush 64, unitary calls).
- Gate: per-driver smoke (amqp-ext auto-skip if the extension is not installed).

### Task 3 — Full run + PR

- Budget unchanged a priori (wide global thresholds); adjustment only if justified.
- Full release run, JSONs archived in the SDD workspace, verify losses=0/dups=0
  (Safe), comparative table 4 drivers × 2 scenarios.
- PR `bench/laravel-realistic` → main.

## Phase B — Publish dead code sweep (MR 2, ~230 lines) — MERGED (PR #33, 2026-08-29)

Executed in SDD (T1 sweep + T2 bench confirmation). Removed: `try_publish_hot` alias,
`PublishPump::try_publish`, `publish_batch_detailed` API + types + tests
(+6/−247, 5 files, 0 verified caller). No-pump else-arms kept (defined behavior,
untouchable contract). Bench confirmation: 15/15 cells within Phase A variance,
0 losses/0 dups, ~3× gap unchanged.

Branch from main. Items identified during the simplification assessment, verified dead
(neither PHP, nor Rust, nor benchmarks):

1. `try_publish_hot` alias (dead since blind routing to the pump).
2. `PublishPump::try_publish` (dead, see pump-v2 Task 4 note: "kept for compat,
   no longer used by the main blind path").
3. Dead else-arm in routing.
4. `mandatory:true` barrier code that became unnecessary.
5. `publish_blind` fallback (dead).
6. `publish_batch_detailed` API (~150 lines with its tests) — no callers.

SDD: T1 removal + caller grep + green tests → T2 confirmation bench
(reuses Phase A scenarios: identical performance expected) → PR.

**Explicitly keep**: pump/actor duality, byte budget/semaphore/ledger,
3 distinct modes (Blind/Unsafe/Safe).

## Phase C — mimalloc A/B (MR 3, after A) — REJECTED (2026-08-29, criterion not met)

- `mimalloc` dependency + `#[global_allocator]` in the cdylib
  (crates/rabbit-rs-php/src/lib.rs). Covers all Rust allocations; Zend MM separate.
- A/B release main vs branch, interleaved: fire-and-forget, batch-confirm,
  auto-ack + the 2 Laravel scenarios (Phase A).
- Additional metric: process **max RSS** (long-lived FPM/Octane) via
  `/usr/bin/time -l` or a probe in the runner.
- Keep criterion: batch-confirm ≥ 5% or meaningfully reduced RSS,
  **without regression** elsewhere; otherwise reject.

Result: interleaved release A/B (main d35580c vs mimalloc 0.1.52, 30 runs, 5 scenarios
× 2 builds × 3 rounds, 0 losses/0 dups) — batch-confirm pub **+4.5% ≤ noise** (< +5%
threshold), RSS **up** (+0.7%→+1.7% medians, 15/15 runs, disjoint ranges, ~+1.6 MB)
instead of down, real regressions worker consume −3.1% / p99 +3.5%. The isolated
auto-ack pub gain (+9.5%, real) falls outside the decision cell. Verdict: rejected,
branch abandoned (local commits `704aaf9`, SDD archives `runs/mimalloc-ab/`).

## Phase D — Backlog (opportunistic) — MERGED (PR #34, 2026-08-29)

Executed in SDD (Tasks D1-D3, ledger `.superpowers/sdd/2026-08-29-post-pump-simplification/`).
D1: `publisher.safety` plumbing (TDD) + docs/wording/stubs + release bench protocol.
D2: invariant tests (closed-pump, pump/Blind debug_assert, non-vacated flush, else=>break
arm proven dead and removed, footgun lib-extension.sh fixed). D3: multi-broker consumer
composition fix (pure rename ConsumerSetHandle + composite fan-in, token-routed acks,
proven TDD e2e). Fix wave: one-shot signal removal (I1) + multi-broker docs (I2). Final
review MERGE-READY, 17/17 checks.

## Phase E — Laravel driver-level benchmark (MR 4, third-party candidates) — MERGED (PR #35, 2026-08-30)

Goal: measure **framework integration** overhead (full Laravel queue API: dispatch, pop,
ack, release/retries) where Phase A measures raw transport. Complements Phase A findings
(is the ~3× publish gap visible once the framework is plugged in?).

### Candidates (assessed 2026-08-29)

1. `goopil/rabbit-rs-laravel` — our driver (native extension, buffered Safe).
2. `vladimir-yuldashev/laravel-queue-rabbitmq` v15.0.1 (2026-08-21) — incumbent,
   php-amqplib transport, adoption reference. **Core of the comparison.**
3. `iamfarhad/laravel-rabbitmq` — modern driver on **ext-amqp** (pooling, confirms,
   quorum, Horizon, Octane). ext-amqp: present on Homebrew php@8.4 (2.2.0) and
   buildable on 8.5 (phpize — pecl/PEAR broken on 8.5); E2 runs archived under Docker
   (php 8.4) with documented local cross-check.
4. Optional: `bschmitt/laravel-amqp` v3.4.1 — wrapper/php-amqplib, transport already
   covered by vladimir-yuldashev, questionable maintenance signals (third-party fork CI
   badge, bloated README). Only include if the skeleton allows it effortlessly.

### Design

- Minimal Laravel app skeleton (composer, single app); 3 coexisting queue
  connections; switching via `QUEUE_CONNECTION`.
- Scenarios aligned with Phase A: **dispatch** (unitary push ×10k) and **worker**
  (pop + unitary ack, identical 1024 B payload). Interleaved runs, release only
  (PHP_CLI: realpath opcache enabled), per-run JSONs archived in the SDD workspace.
- Fairness: each driver's default config documented; confirms variant if
  available (iamfarhad); identical RabbitMQ (same lab, dedicated vhost per driver).
- Environment: goopil + vladimir-yuldashev (php-amqplib) runnable locally;
  iamfarhad requires ext-amqp → Docker lab image (php 8.x + pecl amqp) or dedicated CI job.
- Deliverable: comparative table + written conclusions (where our driver stands at
  framework level), direct input to arbitrate B/C.

### Task E1 — App skeleton + wiring the 3 drivers
### Task E2 — Full run + table + conclusions

### Results (2026-08-30, PR #35)

E2 battery (Docker, 100 runs, 0 losses/dups): dispatch goopil 12 761/s vs
iamfarhad-conf 2 365/s (≥ 5.0×; 5.4× raw); worker goopil 10 030/s median WITH
the stall tax vs 27 073/s on healthy rounds (~4× vladimir despite the stall). Local
ext-amqp cross-check (php 8.4/8.5, same session): same-binary ratio ≈ 7.8×. Framework
safety matrix (interleaved session): blind 76 794/s (2.46× vladimir), unsafe
62 474/s, safe 9 772/s (0.31× — the publish gap is concentrated in the per-message safe
path); consume: leader ~5.9× transport. Three consumer defects documented
with repro harness → round 2 plan `2026-08-30-consumer-stall-and-reliability.md`.
Sonar QG fixes along the way: composer.lock committed (S8567), bench goopil config
reduced to topology override (duplication 30.8% → 0), Docker image non-root
(A security).

## Execution order

A → B → C (A scenarios serve as the measurement baseline for B and C).
E is independent (can run after B; requires Docker/CI for ext-amqp).
D opportunistic. Each phase explicitly validated before execution.

**Final status: A, B, D, E merged; C rejected on data (mimalloc). Plan closed.**
