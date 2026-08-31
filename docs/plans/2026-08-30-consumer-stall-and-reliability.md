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

## Secondary scope (parked items rolled into the round)

1. **Closed-pump batch fail contract test** (`client.rs:143-147`): the batch must
   fail immediately and re-buffer (superset semantics) — ~10 lines, parked since
   Phase B.
2. **`scripts/lib-extension.sh` rebuild-on-change**: a stale artifact is still used
   if it exists (the D2 fix covers build-on-miss + warning only) —
   rebuild when Cargo.toml/lock change.
3. **Symmetric flush_blind test** (`blind_pump.rs`): the blind sibling of the D2
   non-vacated flush test.
4. **shellcheck `scripts/test-integration.sh`** (bash -n only today).
5. **Subscription name uniqueness validation** (pre-existing,
   `update_generation` without a production caller).

## Re-bench (exit criterion)

Full Phase E protocol (100 runs, 4 conditions × 2 modes × 10 rounds,
interleaved, release, archived JSONs) after the fixes. Expected:

- `stall_recoveries = 0` across all worker rounds;
- goopil worker close to the clean round (27 073/s) — gap vs vladimir to re-measure;
- 0 losses / 0 late everywhere (untouchable invariant);
- dispatch unchanged within variance (the fixes only touch the consumer).

Systematic comparison vs E2 archives (`runs/phase-e/` of the SDD workspace — copy the
reference before removing the workspace).

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
   risk — P1 first: biggest measured tax).
2. P3 (dedicated test, fix if confirmed).
3. Secondary scope (items 1-5, independent).
4. Re-bench + E2 comparison.
5. Final review + PR.

Each step explicitly validated before the next.
