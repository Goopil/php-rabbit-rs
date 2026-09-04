# Round I re-bench — post-Round-H/I driver-level and transport-level benchmark

Date: 2026-09-03
Branch: re-bench run on main `a24ef44` + two fixes shipped with this archive
(see "Changes since the round-2 archive").
Broker: local lab, 3-node RabbitMQ 4.2.9 cluster, **fresh state**
(`./scripts/lab-down.sh` (`-v` — volumes wiped) + `./scripts/lab-up.sh with-plugin`
+ `./scripts/lab-ready.sh`).
Extension: **release** cdylib (`target/release/librabbit_rs_php.dylib`, rabbit_rs
0.0.9), loaded per-run with `-d extension=...`, never installed system-wide.
PHP 8.5.6 (cli), Laravel v13.29.0, macOS (Apple Silicon), localhost broker.

## Why this re-bench exists

Round H (v0.1.0 config redesign) and Round I (lost-wakeup fix, settlement
`ConnectionException::throw`, benchmark workarounds deleted) landed on main
after the round-2 archive (2026-08-31). The Round I benchmark runs were
debug-build ad-hoc checks (not comparable). This is the first honest
release-build reference on the post-H/I code.

**The re-bench immediately caught two real defects**, both fixed before this
archive:

1. **Blind/unsafe safety modes were unusable through the Laravel package.**
   The Round H `ConnectionCompiler` derived `mandatory=false` for
   unsafe/blind, but the core config (Round G #78) rejects any
   `mandatory=false` — the field is deprecated; `publisher.safety` is the
   only wire-level opt-out. Every compile with `safety != safe` failed at
   Pool construction ("broker connection failed permanently"). Fixed: the
   compiler now always emits `mandatory=true` (the publisher actor branches
   on the safety mode, never on this flag); the round-2 `goopil-dispatch-blind`
   cell passes again. Pinned by `ConnectionCompilerTest` + `ConnectionConfigCutoverTest`.
2. **The transport harness used the core default `delay.mode = auto`.** Auto
   probes the delay plugin by provisioning `rabbit-rs.delayed`, which the
   lab's vhost "/" configure grant refuses — the probe failure surfaced as
   `FailedPermanent` at consumer acquisition. The harness never publishes
   delayed messages, so it now pins `delay.mode = ttl` (same as the
   driver-bench `config/rabbit-rs.php` override).

Tooling note: `benchmarks/driver-bench` resolves
`goopil/rabbit-rs-laravel` via a path repository with `symlink: false` — the
vendored copy only refreshes on `composer install`/`update`. After changing
the package, delete `benchmarks/driver-bench/vendor/goopil` and re-install,
or benchmarks silently run stale package code.

## Protocol (same shape as round-2)

Interleaved 10-round runs, 3 runs per cell, release build, one archived JSON
per run (`raw/`):

- **Driver-level** (`benchmarks/driver-bench/bin/bench.php`, framework queue
  API, 1024 B Laravel envelope, `--count=1000 --rounds=10`): 5 cells × 3
  interleaved runs = 150 measured rounds. Runner: `scripts/rebench-driver-bench.sh`.
  The workarounds the round-2 archive described are **gone**: there is no
  silent stall-rebuild — a null streak past the plausible bound fails the run
  loudly, so `stall_recoveries = 0` holds by construction.
- **Transport-level** (`benchmarks/src/run-benchmarks.php`, 10 000
  msgs/round × 10 rounds + warmup): 8 cells × 3 interleaved invocations =
  240 measured rounds. Runner: `scripts/rebench-transport.sh`.

## Driver-level results (medians over 30 measured rounds per cell)

| Cell | This session | round-2 archive | Delta |
|---|---|---|---|
| goopil worker (pop+ack) | **16 234 ops/s** | 21 747 | −25 % |
| goopil dispatch blind | 21 992 | 70 262 | −69 % |
| goopil dispatch safe | 6 534 | 7 703 | −15 % |
| vladimir dispatch | 9 685 | 32 193 | **−70 %** |
| vladimir worker | 2 029 | 2 041 | −1 % |

**Read the vladimir column as the session control**: its code is unchanged
third-party PHP, and its dispatch cell dropped −70 % — mirroring the goopil
blind cell (−69 %). The publish-heavy dispatch cells are systematically ~3×
slower this session for BOTH drivers, so the cross-session absolute deltas on
dispatch cells are a session factor, not a code signal.

Same-session ratios (the fair comparison):

| Ratio | This session | round-2 archive |
|---|---|---|
| goopil worker vs vladimir worker | **8.0×** | 10.7× |
| goopil blind vs vladimir dispatch | 2.3× | 2.2× |

The blind ratio is unchanged; the worker ratio moved 10.7× → 8.0× (goopil
−25 % vs vladimir −1 %). Most of that is session drift, but it is the one
cell where a small real per-pop cost from the Round G additions (duplicates
metric, boundary validation) cannot be excluded. **Round D must re-baseline
worker + confirm cells on a fresh lab before/with its profiling** — its
correctness fixes must not hide behind session noise either way.

`losses = 0`, `late_arrivals_after_drain = 0`, `stall_recoveries = 0` in
every one of the 150 rounds; every run's `ok` flag is true.

## Transport-level results (median over 3 interleaved invocations)

| Cell | pub median (min–max) | con median (min–max) | vs amqplib |
|---|---|---|---|
| fire-and-forget / rabbit-rs | 219 713 (194 909–220 922) | 15 703 (14 188–15 785) | pub **5.4×**, con 1.2× |
| fire-and-forget / amqplib | 40 773 (40 098–40 795) | 13 220 (13 184–13 580) | — |
| batch-confirm / rabbit-rs | 31 813 (29 259–32 249) | 40 768 (37 908–41 156) | pub 1.03×, con **4.3×** |
| batch-confirm / amqplib | 30 968 (29 087–31 427) | 9 489 (9 315–9 635) | — |
| laravel-dispatch / rabbit-rs | 15 255 (14 683–15 275) | 38 370 (35 510–39 219) | pub 0.75×, con **4.0×** |
| laravel-dispatch / amqplib | 20 258 (19 851–20 515) | 9 694 (9 572–9 715) | — |
| laravel-worker / rabbit-rs | 191 913 (187 277–193 806) | 12 131 (11 784–12 338) | pub 4.9×, con **6.4×** |
| laravel-worker / amqplib | 38 643 (38 297–39 303) | 1 896 (1 785–1 997) | — |

Compared to the round-2 archive, the quorum confirm-bound cells read much
higher and tighter this session (batch-confirm 24.8k → 31.8k; laravel-dispatch
2.8k → 15.3k) — the round-2 README itself flagged leader placement of a fresh
cluster as the confounder for those cells, and this fresh-lab session
confirms it: the reference levels are reachable and stable on a balanced
cluster. `losses = 0` and `duplicates = 0` in every reliable-mode invocation.

**Round D context (confirmed-publish batching)**: on this fresh session the
confirm-bound rabbit-rs publish cells now match amqplib instead of trailing
it, and the consumer lead is 4–6.4×. The optimization target remains
`laravel-dispatch` publish (unitary confirm+mandatory per message at
15.3k msg/s, 0.75× amqplib) — the only cell where rabbit-rs trails in the
same-session comparison.

## Invariants (hold everywhere)

- 0 losses, 0 duplicates in every reliable-mode run.
- 0 stall recoveries — by construction (workarounds deleted; stalls fail loudly).
- At-least-once delivery with countable duplicates, verified per run.

## Archived artifacts

- `raw/` — one JSON per run (driver-bench: `<cell>-run<N>.json`; transport:
  `transport-<scenario>-<driver>-run<N>.json` + the invocation `.log`).
- `summary.json` — machine-readable medians, min/max, losses, stalls.

## Metric coverage note (Round J, #127)

These archives predate the Round J metric contract (`benchmarks/README.md`).
The `raw/` JSONs record no reconnects; the driver-bench cells additionally
record no per-op latency percentiles and no duplicates, and the transport
cells record no safety/config/meta fields (`summary.json` is derived from
the same raw files). The tooling emits these from Round J on; this archive
is curated evidence and is never backfilled — re-run the cells if a
reconnect- or latency-scoped question matters.

## Reproduce

```bash
./scripts/lab-down.sh && ./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh
rtk cargo build --release -p rabbit-rs-php
./scripts/rebench-driver-bench.sh benchmarks/results/round-i-rebench/raw
./scripts/rebench-transport.sh benchmarks/results/round-i-rebench/raw
./scripts/lab-down.sh
```
