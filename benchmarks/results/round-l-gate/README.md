# Round L gate — final non-regression re-bench vs frozen budgets (plan step F.3)

Date: 2026-10-07. Branch `fix/audit-2026-10-01` at `de18b5b` (freshly pushed; nothing
pulled or pushed for this task). Round L changed hot-path files
(`crates/rabbit-rs-core/src/consumer/actor.rs`, `crates/rabbit-rs-core/src/publisher/actor.rs`,
`crates/rabbit-rs-php/src/classes/publish_buffer.rs`), so the plan's step F.3
requires a fresh driver-bench run compared against the frozen round budgets
before merging. The full quality gate (check.sh 500/500) and
integration/FPM/Octane suites were already green.

## Environment

- MacBook Pro (Apple Silicon), macOS 26.6.2, PHP 8.5.6 (cli, NTS),
  Laravel v13.33.0 (driver-bench `composer.lock`).
- ext-rabbit_rs **0.3.10 release cdylib**
  (`target/release/librabbit_rs_php.dylib`, sha256 `a4c3c4d5…7ebb7b9`, built
  from this exact tree; sources verified not newer than the artifact), loaded
  per-run with `php -n -d extension=<dylib>` — no ini files, never installed
  system-wide. `extension_loaded('xdebug') === false` asserted in the probe;
  per-run stderr captured and **empty across all 16 processes** (15 gate runs
  + probe).
- Built with `--features extension-tests` per the gate instruction: the
  feature (= `rabbit-rs-core/test-support`) gates test-only
  constructors/observers (`with_closed_pump_for_tests`, `buffered_bytes()`,
  …); no measured code path differs from the round-l-profile build (same
  0.3.10 version, built there without the flag).
- Broker: local lab, **fresh** 3-node RabbitMQ 4.2.9 cluster
  (`./scripts/lab-up.sh` — default `with-plugin` profile: delayed-message
  plugin enabled, Prometheus + Toxiproxy up; volumes wiped) +
  `./scripts/lab-ready.sh`. First readiness attempt failed at the 3-node
  cluster check (the management API answered before nodes 2/3 finished
  joining — a startup race in the checker's non-retried step); it passed
  unchanged on a re-run ~1 minute later. No lab or broker configuration was
  changed.
- `bench.goopil.driver-bench` (vhost `/`) verified **classic + durable** via
  the management API before and after the runs.
- The machine was **not quiet**: 1-minute load averages 11.99–15.42 across
  the passes (`load-observations.csv`) — comparable to or above the
  round-l-profile session (5.7–13.1). Per the workload-scoped framing rules
  (`benchmarks/README.md`), medians over interleaved runs and same-session
  ratios are the comparison contract, not absolute cross-session deltas.

## Protocol

Driver-bench standard protocol (`benchmarks/driver-bench/bin/bench.php`,
framework queue API, 1024 B Laravel envelope, `--count=1000 --rounds=10`):
**5 cells × 3 interleaved passes = 150 measured rounds**, one JSON per run
under `raw/`. Cells and order per pass (same set as round-2/round-i):

1. `goopil-dispatch-blind` (`RABBIT_RS_SAFETY=blind`, unit `Queue::push`)
2. `goopil-dispatch-safe` (`RABBIT_RS_SAFETY=safe`, confirms + mandatory)
3. `goopil-worker` (fill unmeasured, measured unit pop+ack, prefetch 64)
4. `vladimir-dispatch` (unchanged third-party control)
5. `vladimir-worker` (unchanged third-party control)

Runner: `run-gate.sh` in this directory (shape of
`scripts/rebench-driver-bench.sh` under the round-l-profile execution
discipline). Session metadata: `provenance.json`; per-pass load:
`load-observations.csv`.

### Configuration parity with the frozen rounds

- `delay.mode=ttl` (the archive pin — no delayed exchange is ever declared).
- Prefetch 64, `topology_mode=declare`, 1024 B envelope
  (`payload_body_bytes=1024` in every run), `count=1000`, `rounds=10`.
- **Classic durable queue — measurement-configuration patch.** The committed
  bench config (`benchmarks/driver-bench/config/rabbit-rs.php`) ships a
  nested `'topology'` block that `ConnectionCompiler::topology()` does not
  read (it reads the flat `queue_type`/`queue_durable` keys; the package
  default is `queue_type=quorum` since the Round H connection-first
  redesign). Left as committed, the bench silently declares **quorum**
  queues — a different workload from every frozen archive (all classic; the
  driver-bench fairness table documents classic as the intended bench
  config). Exactly as `results/round-l-profile` did, an **uncommitted** flat
  `'queue_type' => 'classic'` key was applied for the measurements and
  **reverted before committing this archive**. Evidence: the probe's config
  echo (`raw/probe-config-echo.json`, `rabbit_rs_global.queue_type=classic`,
  `delay.mode=ttl`) and the broker-side queue declaration (classic, durable).
- Vendored `goopil/rabbit-rs-laravel` refreshed before the runs
  (`composer update goopil/rabbit-rs-laravel --ignore-platform-req=ext-amqp`);
  `vendor/…/src` verified **byte-identical** to `packages/laravel-queue/src`
  afterwards (the stale-vendor trap documented in round-i and
  round-l-profile). The refresh bumped the committed `composer.lock`'s
  path-repo reference (0ac0aad → 808bee4); the lock was restored to its
  committed state after the runs so this archive commits cleanly — re-running
  `composer update goopil/rabbit-rs-laravel` in `benchmarks/driver-bench`
  reproduces the used state.

## Results (round-median over the 30 measured rounds per cell)

| Cell | This session | round-l-profile (2026-10-04, like-for-like) | Δ vs profile | round-2 re-bench (2026-08-31) | Δ vs round-2 | round-i (2026-09-03) | round-d pipelined (2026-09-04) | Verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| goopil-dispatch-blind | **73 233** | 71 380 | +2.6 % | 70 262 | +4.2 % | 21 992 (session factor documented −69 %) | 21 939 | **PASS** |
| goopil-dispatch-safe | **71 498** | 65 753 | +8.7 % | 7 703 (pre-pipelined semantics) | — | 6 534 (pre-pipelined) | 20 866 | **PASS** |
| goopil-worker (pop+ack) | **30 719** | 23 495 | +30.8 % | 21 747 | +41.3 % | 16 234 | 15 421 | **PASS** |
| vladimir-dispatch (control) | **33 412** | — | — | 32 193 | +3.8 % | 9 685 (session factor) | 9 545 | **PASS** |
| vladimir-worker (control) | **2 172** | — | — | 2 041 | +6.4 % | 2 029 | — | **PASS** |

Per-cell min/max across the 30 rounds (ops/s) and per-run aggregates
(median of the 3 run averages in parentheses):

| Cell | round min | round max | run avgs (median) | p50 | p95 | p99 |
|---|---:|---:|---|---:|---:|---:|
| goopil-dispatch-blind | 67 446 | 75 146 | 73 163 / 72 985 / 72 904 (72 985) | 13 µs | 16 µs | 24 µs |
| goopil-dispatch-safe | 57 284 | 75 544 | 69 892 / 68 051 / 72 630 (69 892) | 13 µs | 17 µs | 27 µs |
| goopil-worker | 25 182 | 36 248 | 30 141 / 29 675 / 30 411 (30 141) | 7 µs | 25 µs | 290 µs |
| vladimir-dispatch | 32 281 | 34 617 | 33 535 / 33 571 / 33 185 (33 535) | 28 µs | 39 µs | 72 µs |
| vladimir-worker | 1 709 | 2 302 | 2 090 / 2 148 / 2 099 (2 099) | 441 µs | 658 µs | 1 057 µs |

Reading the comparisons (workload-scoped framing only — unit `Queue::push` /
unit pop+ack, 1024 B Laravel envelope, classic durable queue, this lab, this
session):

- **Every cell is at or above its round-l-profile level** (same round, same
  protocol, same machine, taken at `936c062` before the Round L hot-path
  commits): blind +2.6 %, safe +8.7 %, worker +30.8 %. All are inside or
  above the profile's own pass spreads (blind ±0.4 %, safe ±2 %, worker wide
  — its pass 2 was a background-noise outlier) with the session load noted
  above. No cell is within a mile of a regression signal.
- **Same-session ratios vs the unchanged vladimir control** (the fair
  cross-session comparison): blind/vladimir-dispatch **2.19×** (frozen
  2.2–2.3×, round-2/round-i — unchanged); worker/vladimir-worker **14.1×**
  (frozen 8.0–10.7× — above the frozen range; the vladimir controls
  themselves read +3.8 %/+6.4 % vs round-2, confirming a comparable session,
  so the worker move is not a control artifact).
- **The worker cell** reaches 30 719 ops/s against the frozen budget the
  gate anchors on (round-2 median 21 747, taxed pre-fix baseline 10 030) —
  +41 % vs the frozen budget and ~3.1× the taxed baseline, with the stall
  tax still absent (`stall_recoveries = 0` everywhere).
- **The safe publish cell** (pipelined safe flush) reaches 71 498 ops/s
  against the round-d pipelined reference 20 866 and sits at blind parity
  (safe/blind = 0.98), as in the profile.

## Invariants (blocking contract — hold everywhere, 15/15 runs)

| Counter | Observed | Contract |
|---|---|---|
| `ok` | true in 15/15 runs | must be true |
| `losses` | **0** in 15/15 runs | 0 |
| `late_arrivals_after_drain` | **0** in 15/15 runs | 0 |
| `stall_recoveries` | **0** across all 150 rounds | 0 (by construction — stalls fail loudly) |
| `duplicates` | **0** in all worker runs (dispatch cells: `null` — nothing consumed, per the metric contract) | 0 where measured |
| `reconnects_total` | **0** in all rabbit-rs runs (`null` for vladimir — not surfaced) | — |

No run failed, no retry was needed, no error JSON was produced; total stderr
across all runs is 0 bytes.

## Verdict

**GATE PASS.** All five cells satisfy the fixed decision rule (declared in
`compute-summary.php` before the numbers were computed): invariants intact;
every cell above its lowest frozen classic-queue median
(21 939 / 20 866 / 15 421 / 9 545 / 2 029); every cell within 25 % of — in
fact above — its round-l-profile level; same-session ratios inside/above the
frozen ranges. The Round L hot-path changes
(consumer/publisher actor observability & hardening, publish-buffer stub
alignment) show **no throughput regression** on the driver-bench standard
protocol, and the delivery contract (at-least-once, 0 losses / 0 duplicates /
0 stalls / 0 reconnects) held in every measured round.

## Deviations from the letter of the task (all disclosed, none affect the verdict)

1. **Uncommitted bench-config patch during the runs** (flat
   `queue_type=classic`, next to the dead nested block) — required to match
   the frozen rounds' configuration; same procedure as round-l-profile;
   reverted before committing.
2. **`composer.lock` restored** after the vendor refresh (the refresh is
   required by the protocol; the lock bump is mechanical — see Configuration
   parity above).
3. **Extension built with `--features extension-tests`** per the gate
   instruction (test-only code; see Environment).
4. **Vladimir controls ran under `php -n`** (older archives used the default
   ini) so all five cells share one measured environment.
5. `lab-ready.sh` needed one re-run after a cluster-formation startup race
   (see Environment) — no configuration or code involved.

## Artifacts

- `raw/` — one JSON per run (`<cell>-run{1,2,3}.json`), one `.stderr.log` per
  run (all empty), and the probe (`probe-config-echo.json` — config parity
  evidence, excluded from the measured set).
- `summary.json` — machine-readable medians, min/max, invariants, per-frozen
  deltas, ratios, verdicts (derived by `compute-summary.php`).
- `load-observations.csv` — load bracketing every pass.
- `provenance.json` — session provenance (git, extension, lab, config,
  deviations).
- `run-gate.sh`, `compute-summary.php` — the runner and the summary/verdict
  computation.

## Reproduce

```sh
./scripts/lab-up.sh && ./scripts/lab-ready.sh        # fresh lab (volumes wiped)
rtk cargo build -p rabbit-rs-php --release --features extension-tests
cd benchmarks/driver-bench && composer update goopil/rabbit-rs-laravel \
  --ignore-platform-req=ext-amqp && cd ../..
# with the flat 'queue_type' => 'classic' patch applied to
# benchmarks/driver-bench/config/rabbit-rs.php (see Configuration parity):
./benchmarks/results/round-l-gate/run-gate.sh
php benchmarks/results/round-l-gate/compute-summary.php
# then revert the config patch and the composer.lock reference
./scripts/lab-down.sh
```

## Metric coverage note

All JSONs follow the Round J metric contract (`benchmarks/README.md`):
per-op latency percentiles with the measured call named in
`latency_ms.source`, duplicates (worker cells; `null` in dispatch),
`reconnects_total` (rabbit-rs; `null` for vladimir), masked config echo and
`meta` (php/rabbit_rs/os) per run.
