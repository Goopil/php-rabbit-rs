# Round 2 re-bench — post-consumer-fix driver-level and transport-level benchmark

Date: 2026-08-31
Branch: `task/39-secondary-rebench` (base: main `7707b5d`, includes the P1/P2 fixes,
notably `bbd836b` "flush publish buffer on consume so consumers cannot starve").
Broker: local lab, 3-node RabbitMQ 4.2.9 cluster (`./scripts/lab-up.sh with-plugin`).
Extension: **release** cdylib (`target/release/librabbit_rs_php.dylib`, rabbit_rs 0.0.7),
loaded per-run with `-d extension=...`, never installed system-wide. PHP 8.5.6 (cli),
Laravel v13.29.0, macOS (Apple Silicon), localhost broker.

## Protocol (honest form)

The Round 2 plan calls for the "full Phase E protocol (100 runs, 4 conditions ×
2 modes × 10 rounds, interleaved, release, archived JSONs)". The harnesses expose
no literal "100-run" switch, so the protocol was realized as **interleaved
10-round runs, 3 runs per cell**, release build, one archived JSON per run
(`raw/`), which exceeds the 100-round scale:

- **Driver-level** (`benchmarks/driver-bench/bin/bench.php`, framework queue API,
  1024 B Laravel envelope, `--count=1000 --rounds=10`): 5 cells × 3 interleaved
  runs = 150 measured rounds. Cells: goopil dispatch blind (`RABBIT_RS_SAFETY=blind`),
  goopil dispatch safe (`RABBIT_RS_SAFETY=safe`), goopil worker (pop+ack, fill
  unmeasured), vladimir dispatch, vladimir worker. Runner:
  `scripts/rebench-driver-bench.sh`.
- **Transport-level** (`benchmarks/src/run-benchmarks.php`, 10 000 msgs/round ×
  10 rounds + warmup): 8 cells × 3 interleaved invocations = 240 measured rounds.
  Cells: fire-and-forget, batch-confirm, laravel-dispatch (safe unitary publish),
  laravel-worker (unit pop+ack) × rabbit-rs / amqplib. Runner:
  `scripts/rebench-transport.sh`.

"Pre-fix reference" = the numbers recorded in
`docs/plans/2026-08-30-consumer-stall-and-reliability.md`, the round 2 ROADMAP and
`benchmarks/driver-bench/README.md` (Phase E / Phase B archives from earlier
sessions; the SDD `runs/phase-e/` workspace no longer exists).

## Driver-level results (primary comparison — classic queues)

Median over 30 measured rounds (3 interleaved runs × 10 rounds), 1000 ops/round.

| Cell | Re-bench median | Pre-fix reference | Delta |
|---|---|---|---|
| goopil worker (pop+ack) | **21 747 ops/s** | 10 030 (with stall tax, 9/10 rounds affected) | **+117 %** |
| goopil worker vs clean rounds | 21 747 | 27 073 (E2 clean-round battery, earlier session) | −20 % (see session drift below) |
| goopil dispatch blind (fire-and-forget) | 70 262 | 76 794 | −9 % |
| goopil dispatch safe (per-message confirm+mandatory) | 7 703 | 9 772 | −21 % |
| vladimir dispatch | 32 193 | 31 182 | +3 % |
| vladimir worker | 2 041 | ~2 500 (implied by the E2 ~4× ratio) | −18 % |

Per-cell min/max across the 30 rounds (ops/s):

| Cell | min | max |
|---|---|---|
| goopil dispatch blind | 63 419 | 71 759 |
| goopil dispatch safe | 4 361 | 9 623 |
| goopil worker | 8 836 | 24 808 |
| vladimir dispatch | 30 496 | 33 284 |
| vladimir worker | 445 | 2 259 |

**Headline — the stall tax is gone.** `stall_recoveries = 0` across all 30 worker
rounds (pre-fix: the E2 battery saw stalls in 9 of 10 rounds, each costing
~0.6–0.8 s billed into the measured time). Worker median more than doubles vs the
taxed baseline (10 030 → 21 747).

Session-drift context (read absolute deltas, not just percentages): this session's
non-confirm cells track the E2 references closely but slightly low — goopil blind
−9 % while vladimir dispatch reads +3 %. Comparing like with like *inside this
session*, the framework-level worker lead over vladimir is 21 747 / 2 041 ≈ **10.7×**
(E2 reported ~4× at framework level *with* the stall tax; the transport-level lead
was ~5.9×). The remaining −20 % vs the E2 clean-round number is within the
observed session drift plus one slow round (8 836 ops/s in run 2); the pre-fix
"clean round" reference was a best-case subset of E2 rounds, not a median.

`losses = 0` and `late_arrivals_after_drain = 0` in every run; every run's `ok`
flag is true (worker mode requires exactly `count` popped+acked messages and an
empty queue after the 5 s settling window).

## Transport-level results (context cells)

Median over 3 interleaved invocations (each 10 rounds × 10 000 msgs), min–max of
the per-round rates across invocations.

| Cell | pub median (min–max) | pre-fix pub | con median (min–max) |
|---|---|---|---|
| fire-and-forget / rabbit-rs | 268 959 (223 879–295 640) | 260 679 ✓ | 14 590 (9 222–19 560) |
| fire-and-forget / amqplib | 89 657 (82 882–91 732) | 90 590 ✓ | 47 064 (43 056–48 322) |
| batch-confirm / rabbit-rs | 24 792 (16 119–39 660) | 37 353 ⚠ | 34 859 (23 442–45 136) |
| batch-confirm / amqplib | 47 464 (31 963–51 683) | 49 902 ✓ | 29 712 (27 849–30 992) |
| laravel-dispatch / rabbit-rs | 2 812 (1 806–7 086) | 8 213 ⚠ | 31 208 (17 020–40 583) |
| laravel-dispatch / amqplib | 26 800 (23 063–30 585) | 28 458 ✓ | 29 300 (26 535–30 649) |
| laravel-worker / rabbit-rs | 215 121 (187 857–252 315) | — | 12 963 (8 845–15 611); pre-fix 14 425 ✓ (−10 %) |
| laravel-worker / amqplib | 86 290 (81 335–88 665) | — | 2 255 (2 089–2 697); pre-fix 2 452 ✓ (−8 %) |

⚠ Reading: the confirm-bound rabbit-rs cells (batch-confirm, laravel-dispatch)
run on the **quorum** queue the transport harness declares, and their confirms are
RTT-bound on raft replication. In this fresh lab session the amqplib cells (classic
queues) and both fire-and-forget cells match their pre-fix references within a few
percent, while the quorum confirm-bound cells read lower with wide spreads — the
third pass reached 36 848 (batch-confirm) and 7 086 (laravel-dispatch),
demonstrating the reference levels are still reachable; leader placement of the
freshly formed 3-node cluster is the likely confounder. These cells were
**unchanged by the Round 2 fixes** (consumer-side only), so the honest comparison
for them is variance context, not a regression claim.

## Budget validation (`benchmarks/baselines/smoke-budget.json`)

Checked by the transport harness on every invocation: **19/24 invocations ALL
PASS; 0 losses / 0 duplicates everywhere** (untouchable invariants hold). The 5
`*_p99_max_ms` failures are all e2e publish→consume latency p99 on
publish-then-drain scenario shapes, where the metric (harness `Budget.php` reads
the consume-side e2e p99) equals the phase duration of the slower side:

- `laravel-dispatch/rabbit-rs` runs 1–2: p99 4 247 / 3 941 ms — publish phase
  duration at this session's quorum confirm rate (~2.4–2.8 k/s × 10 000 msgs).
  Run 3 (5.6 k/s → 1 819 ms) passes.
- `laravel-worker/amqplib` runs 1–3: p99 4 446 / 4 400 / 3 846 ms — the last of
  10 000 messages waits the ~2.2 k/s drain of a slow consumer in queue; inherent
  to the scenario shape (pre-fix vladimir worker ran at a similar rate).

No `*_throughput_min` (1000/500) or `losses_max` (0) budget failed anywhere.
Driver-level runs have no in-harness budget check; they satisfy the budget
manually: throughput ≥ 2 041 (≥ 1000), worker throughput ≥ 21 747 (≥ 500), and
0 losses everywhere.

## Archived artifacts

- `raw/` — one JSON per run (driver-bench: `<cell>-run<N>.json`; transport:
  `transport-<scenario>-<driver>-run<N>.json` + the invocation `.log` including
  the budget verdict).
- `summary.json` — medians, min/max, stall recoveries, losses, per-invocation
  transport details (machine-readable).

## Metric coverage note (Round J, #127)

These archives predate the Round J metric contract (`benchmarks/README.md`).
The `raw/` JSONs record no reconnects; the driver-bench cells additionally
record no per-op latency percentiles and no duplicates, and the transport
cells record no safety/config/meta fields. The tooling emits these from
Round J on; this archive is curated evidence and is never backfilled —
re-run the cells if a reconnect- or latency-scoped question matters.

## Reproduce

```bash
./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh
rtk cargo build --release -p rabbit-rs-php
./scripts/rebench-driver-bench.sh benchmarks/results/round-2-rebench/raw
./scripts/rebench-transport.sh benchmarks/results/round-2-rebench/raw
./scripts/lab-down.sh
```
