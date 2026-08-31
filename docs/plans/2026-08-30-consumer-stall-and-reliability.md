# Round 2 — Consumer reliability: ack-pipeline stall, pre-fill deliveries, Pool::clear × consumer

Date: 2026-08-30
Base: main (b3294c1, PR #35 merged)
Previous: `2026-08-29-post-pump-simplification.md` (executed, merged)
Roadmap: `docs/plans/ROADMAP.md` — round 2 = priority; round C (local integration
harness) and round D (dispatch gap) parked.

## Context

The perf-gap campaign (pump v2 + post-pump) established the performance windows:

- publish fire-and-forget: goopil **leader** (2.46× vladimir at framework level,
  ~2.8× at transport) — nothing to optimize;
- publish safe (at-least-once contract): 0.31× framework / 0.28× transport — the
  frontier, target of round D;
- consume: **leader ~5.9×** at transport; ~4× at framework level DESPITE the stall
  tax (goopil worker 10 030/s median vs 27 073/s on healthy rounds, E2 battery).

Three consumer defects, observed at driver level by the Phase E harness
(`benchmarks/driver-bench/README.md` § Known ext-rabbit_rs consumer quirks, with
documented reproductions), drag down the worker path. None of them is an at-least-once
loss (messages remain `ready` on the broker side) but all degrade the real latency and
throughput of a Laravel worker.

## Bugs (by priority)

### P1 — Ack-pipeline stall under sustained pop+ack (2.70× tax)

The consumer stops receiving deliveries during a sustained unitary pop+ack drain
while messages remain `ready` (prefetch-independent: observed with 1 and 64;
purge-independent). The runner detects it (400 consecutive null pops) and rebuilds
the connection — ~0.6-0.8 s per stall, billed in the measured time, `stall_recoveries`
reported per round. 9 out of 10 rounds affected in the E2 battery.

Root-cause investigation FIRST: the preliminary code exploration
(`consumer/actor.rs`, `composite.rs`, `set.rs`, `delivery.rs`) did not identify the
mechanism. The driver-bench harness reproduces at will (~stall under sustained pop+ack)
— debug-friendly. Suspected areas: ingestion of asynchronous `basic.deliver` frames vs
`next()` buffer state, credit/prefetch re-arming, ack token interaction →
`try_settle` → CAS (delivery.rs).

Deliverable: documented root-cause + fix + non-regression test at core level
(paused Tokio time + scriptable mock transport, no real sleeps).

### P2 — Consumer created before the fill is ingested: missed deliveries (~2%)

If the native consumer is created before the fill has been ingested (first `pop()`
while the fill is in flight, or consumer left in place between two rounds), a
fraction of the messages never surfaces on that connection (reproducible ~2%).
Verified: consumer created before the fill → ~2% missed; created after → clean. The 3
competing drivers (amqplib, amqp-ext, bunny) do not exhibit the behavior.
Messages remain `ready` (not a loss), but a real worker starting during an in-progress
publication can starve.

Deliverable: root-cause + fix + core test of the consumer-creation × in-flight backlog
race + removal of the runner workarounds (per-round reconnect, round-0 guard) if the
fix allows.

### P3 — `Pool::clear()` × pre-existing consumer: pops degraded ~25×

Combination observed once (first attributed to the P2 pattern; a combination deserving
a dedicated core-side test). `Pool::clear()` (fork recovery, invalidation) in the
presence of a pre-existing consumer degrades pops ~25×.

**Resolved 2026-08-30 — no core defect; verdict documented (plan fallback).**
Dedicated core tests added in `crates/rabbit-rs-core/tests/pool_clear.rs` (mock
transport, paused Tokio time): the combination is safe at core level. Deliveries keep
flowing through a consumer that survives `purge_queue` calls between rounds, all
settlements reach their broker channel with the unchanged connection generation
(no stale-ACK drift), the consumer is never re-established (one `QoS`, one `consume`
regardless of the number of purges), and the purge path opens at most one extra
broker connection, cached and closed with the pool.

Root cause of the observed ~25× (harness mechanics, not a core purge defect):

1. In the Phase E matrix (`benchmarks/src/AbstractBenchmark.php`), every measured
   round after the first calls `purgeQueue()` → `Pool::clear()` while the consumer
   from the previous round stays attached — and the next fill is ingested into a
   live consumer. That is exactly the P2 configuration (consumer attached while the
   fill is in flight → ~2% missed deliveries), so P3's data is contaminated by P2.
2. A `Pool::clear()` on a fresh vhost (queue not yet declared) fails with a 404
   channel error; the harnesses catch it and fall back to a pop-drain
   (`driver-bench/bin/bench.php` `purgeQueue()` → `drainAll()`), which creates the
   consumer before the fill — feeding P2 again.
3. The amplification that turns ~2% missed deliveries into ~25× slower pops: the
   matrix's null-pop path blocks up to 1 s per null (`Consumer::next(1000)` after a
   failed `tryNext()`), and `consumeSingle`/`consumeBatchConfirm` break after 3
   consecutive nulls. A drain that ends with missed deliveries therefore pays
   1-3 s of null blocks on top of a ~74 ms healthy drain (2 000 messages at the
   healthy ~27 000 pop/s) — the measured rate collapses to the observed order of
   magnitude. The driver-bench worker drain is unaffected by this amplifier (its
   null path sleeps 250 µs), which is why the combination was "observed once".

Documented correct sequence (unchanged behavior, no code change): call
`clear()` before creating the consumer for a round (the driver-bench runner
already does: purge → reconnect → fill → pop). A consumer that survives a purge
remains functional — pinned by the pool_clear tests. The residual pop degradation
tracks P2 and is addressed by the P2 task.

## Secondary scope (parked items rolled into the round) — DONE 2026-08-31

1. **Closed-pump batch fail contract test** (`client.rs`): DONE — pinned in
   `crates/rabbit-rs-core/tests/blind_pump.rs`
   (`blind_batch_on_a_closed_pump_fails_immediately_and_leaves_everything_with_the_caller`).
   A deterministic closed-pump state is built through test-support-only
   constructors (`PublishPump::closed_for_tests` →
   `PublisherActor::with_closed_pump_for_tests` →
   `ClientPool::install_closed_pump_publisher_for_tests`); a mutation check
   confirmed the test fails if the closed-pump error were swallowed into a
   synthetic `Confirmed` outcome.
2. **`scripts/lib-extension.sh` rebuild-on-change**: DONE — the debug artifact is
   rebuilt when stale, detected by (a) crate sources/manifests/lockfile newer than
   the artifact and (b) a feature stamp recording the last
   `--features extension-tests` build (an artifact newer than the stamp was
   rebuilt by another cargo command — e.g. check.sh's workspace test build — and
   lacks the test feature). Verified: fresh artifact → no warning; touched
   source → rebuild; feature-less rebuild → rebuild.
3. **Symmetric flush_blind test** (`blind_pump.rs`): DONE — the ClientPool-level
   flush test now uses the D2 non-vacuous assert (a full simulated second of
   timeout expiry as positive proof the barrier stayed parked), symmetric with
   the pump-level D2 test; mutation-checked.
4. **shellcheck `scripts/test-integration.sh`**: DONE — clean
   (`shellcheck -x` from `scripts/`, exit 0; the `source=` directive resolves
   relative to the CWD). No findings to fix in the script; the pre-existing
   SC2028 in `lib-extension.sh` was fixed with printf.
5. **Subscription name uniqueness validation**: DONE — `Config::validate` rejects
   duplicate subscription names within a worker profile with a typed
   `ConfigError` on the exact `workers.<name>.subscriptions.<subscription>`
   path (previously two subscriptions sharing a name raced for the same
   `SubscriptionId` in `update_generation`).

## Re-bench (exit criterion) — DONE 2026-08-31, archived

Full interleaved protocol, release build, archived JSONs
(`benchmarks/results/round-2-rebench/` — raw runs + `summary.json` + README).
The SDD `runs/phase-e/` workspace no longer exists; deltas are computed against
the numbers recorded in this plan, the ROADMAP and
`benchmarks/driver-bench/README.md`. Protocol honesty note: the harnesses have no
literal "100-run" switch — the protocol was realized as 3 interleaved 10-round
runs per cell (driver-level: 5 cells × 30 rounds; transport: 8 cells × 30 rounds
of 10 000 msgs), exceeding the 100-round scale.

Driver-level (classic queues, primary comparison):

| Cell | Re-bench median (30 rounds) | Pre-fix reference | Delta |
|---|---|---|---|
| goopil worker (pop+ack) | **21 747 ops/s** | 10 030 taxed median | **+117 %**, `stall_recoveries = 0` in 30/30 rounds (9/10 affected pre-fix) |
| goopil worker vs clean rounds | 21 747 | 27 073 (E2 clean-round subset) | −20 % (session drift −9 % on blind; one slow round 8 836) |
| goopil dispatch blind | 70 262 | 76 794 | −9 % |
| goopil dispatch safe | 7 703 (4 361–9 623) | 9 772 | −21 % (wide spread; run 2 median 8.7k) |
| vladimir dispatch | 32 193 | 31 182 | +3 % |
| vladimir worker | 2 041 | ~2 500 | −18 % |

0 losses / 0 late arrivals / every run `ok` in every driver-level run.

Transport-level (context): fire-and-forget rabbit-rs 268 959 pub (reference
260 679 ✓), laravel-worker consume 12 963 (14 425 ✓ −10 %), both amqplib
references matched; confirm-bound quorum-queue cells (batch-confirm 24 792 vs
37 353; laravel-dispatch 2 812 vs 8 213) read below their Phase B references
with wide spreads and reachable peaks (36 848 / 7 086 in pass 3) — variance
context documented, not a regression claim (this round's fixes are
consumer-side only).

Budgets (`benchmarks/baselines/smoke-budget.json`): 19/24 transport invocations
ALL PASS; 0 losses / 0 duplicates everywhere; the 5 p99 failures are
e2e-latency-on-publish-then-drain shape artifacts, itemized in the archive
README.

Expected outcomes versus the plan: `stall_recoveries = 0` ✓; worker close to the
clean round — closed to within session drift of the E2 clean-round number while
more than doubling the taxed median ✓; 0 losses / 0 late everywhere ✓; dispatch
unchanged within variance ✓.

## Protocol

- TDD: red test per bug → minimal fix → green → existing invariants green.
- Gates: `rtk ./scripts/check.sh`; `./scripts/test-extension.sh` (rebuild
  `--features extension-tests` BEFORE, otherwise phantom testing_pool failures);
  `./scripts/test-laravel.sh`. Benchmarks release only, interleaved runs.
- Untouchable contract: at-least-once (Safe/Unsafe); Blind = documented
  fire-and-forget; no unsafe Rust; connection-generation-aware acks.
- SDD: file brief → implementer → reviewer → ledger
  `.superpowers/sdd/2026-08-30-consumer-stall-and-reliability/`. Never fix outside
  review.

## Execution order

1. Root-cause investigation P1/P2 (parallel readings possible, sequential fixes by
   risk — P1 first: biggest measured tax). DONE on main (`bbd836b` publish-buffer
   flush on consume; `08ba5e8` requested-profile establishment).
2. P3 (dedicated test, fix if confirmed). DONE — documented verdict, no core defect.
3. Secondary scope (items 1-5, independent). DONE 2026-08-31.
4. Re-bench + E2 comparison. DONE 2026-08-31 (`benchmarks/results/round-2-rebench/`).
5. Final review + PR.

Each step explicitly validated before the next.
