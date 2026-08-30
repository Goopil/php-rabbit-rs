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
2. **P2 — pre-fill missing deliveries (~2%).** If the native consumer is created
   before the fill has been ingested (first `pop()` while the fill is in flight, or a
   consumer left idle across rounds), a fraction of messages never surfaces on that
   connection. Verified: consumer created pre-fill → ~2% missed; created after the
   fill → clean. Other drivers (amqplib, amqp-ext, bunny) are unaffected.
3. **P3 — `Pool::clear()` with a pre-existing consumer.** Combo degrades pops ~25×.
   Needs a dedicated core-level test.

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

## Housekeeping (owner actions)

- `git pull` the main checkout (`~/dev/perso/rabbit-rs` is behind `origin/main`).
- Delete the orphan SonarCloud project `Goopil_rabbit-rs` and the now-unused
  `SONAR_TOKEN` secret (coverage goes to Codecov only; SonarCloud runs automatic
  analysis on `Goopil_php-rabbit-rs`).
