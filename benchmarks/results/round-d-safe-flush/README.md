# Round D — pipelined safe flush: root cause, implementation, re-bench (issue #41)

Date: 2026-09-04
Branch: `task/41-round-d`, profiling at `5df69ba` (+ experiment commit `b1b6baa`),
implementation as of `5f79851`.
Broker: local lab, 3-node RabbitMQ 4.2.9, fresh cycle for each measurement
session (`./scripts/lab-down.sh` + `./scripts/lab-up.sh with-plugin` +
`./scripts/lab-ready.sh`).
Extension: **release** cdylib (`target/release/librabbit_rs_php.dylib`,
rabbit_rs 0.1.0), loaded per-run with `-d extension=...`, never installed
system-wide. PHP 8.5.6 (cli), Laravel v13.29.0, macOS (Apple Silicon),
localhost broker.
Driver-level cells (`benchmarks/driver-bench/bin/bench.php`, framework queue
API, 1024 B Laravel envelope, `--count=1000 --rounds=10`), 3 runs × 10 rounds
per cell, **round-median over the 30 measured rounds**. `run-cell.sh` in this
directory is the runner (goopil cells); the vladimir control cell is the same
command with `--connection=rabbitmq-amqplib`.

## 1. Phase 1 — safe publish path root cause

Baseline (same session, official 3×10 protocol, round-median, `baseline/`):

| Cell | rate | round-i archive | note |
|---|---|---|---|
| goopil-dispatch-blind | 21 654 | 21 992 | matches |
| **goopil-dispatch-safe** | **5 729** | 6 534 | the target |
| goopil-worker | 13 651 | 16 234 | |
| vladimir-dispatch (control) | 9 615 | 9 685 | matches → session comparable |

Safety ladder (probe, same session): blind 21 036 → unsafe 17 782 → safe
5 445 msg/s. Safe-mode confirm RTT (actor-perceived,
`confirmation_latency`): p50=3 ms, p95=4, p99=6–9.

**Refuted as stated:** safe mode does NOT await one confirm per message
(that would cap at 1/1.8 ms ≈ 550 msg/s; measured 5.3–6k). Within one flush
batch, `publish_batch` releases all messages (`try_publish`) before awaiting
outcomes (`PublishWaiter::wait_all`) — the Transport API is used as designed.

**Confirmed in batch form:** the PHP-side `PublishBuffer` flush
(`block_on(publish_batch)`) serializes each batch's confirm-wave against
production. Measured with the `RS_PROF` probe (experiment commit `b1b6baa`):

- Batch size is window-limited: interval 1 ms × intrinsic PHP produce rate
  (~24–25k/s) → batch p50 = 24–25 msgs.
- `block_on` per flush: T_flush ≈ 2.0 ms + 36 µs × N (fits N=25/48/64/1024:
  3.0 / 4.4 / 4.4 / 32.2 ms per batch). N=1 flush = 1.8 ms (pure
  single-confirm RTT; broker group-commit on the Docker volume); 36 µs/msg
  marginal = actor + confirm machinery + cross-thread wake chain (unsafe
  measures 5.2 µs/msg for the same path minus confirms).
- At baseline, `block_on` = **74 % of wall time**; `sample` shows the main
  thread 63 % parked in `_pthread_cond_wait` (waiting for confirms) while
  the tokio worker is 97 % in `kevent` and the lapin io loop 97.5 % in
  `semaphore_wait` — latency-bound, not CPU-bound.
- Confirms arrive asynchronously at p50=3 ms latency but ~21k/s sustained —
  the pipeline overlaps RTTs fine; the barrier is what serializes.

Experiments (`b1b6baa` env knobs):

| Experiment | result |
|---|---|
| tokio workers 1→4 (`RS_TOKIO_WORKERS`) | no delta (×1.0): threads idle, CPU not the constraint |
| deeper window (`RS_FLUSH_MS` 1→2→4→0, `RS_FLUSH_MAX` 64→256→1024) | 6.0k → 7.3k → 8.9k → 12.1k; saturates at the barrier+produce asymptote (~13k) |
| **barrier removal** (`RS_ASYNC_FLUSH=1`, contract-violating prototype) | safe dispatch **5.3k → 21.5k (×4.05)**, official 3×10 protocol, ok=1 everywhere |
| property-clone dedupe | bound ≤ a few µs/msg — not a lever at 15–25k rates |

Ranked causes: (1) `block_on` flush barrier — 74 % of wall time, ×4.05 when
removed, THE root cause; (2) small in-flight window — subsumed by (1); (3)
more tokio workers — dead end; (4) per-message clones — dead end; (5) ~2 ms
single-confirm RTT — broker-side group-commit on this lab, overlaps away
under (1), amqplib parity confirms the broker sustains ≥20k/s confirmed.

## 2. Phase 2 — implementation (contract restored)

The prototype's ×4.05 came from discarding outcomes. The shipped
implementation keeps the pipeline and restores every contract it dropped
(design note: `docs/superpowers/specs/2026-09-04-pipelined-safe-flush-design.md`):

- **Pipelined flush**: `Pool::publish` triggers a drain that spawns on the
  tokio runtime and returns before confirmations resolve. The buffer keeps
  its ceilings (4096 msgs / 64 MB) and the 64-message / 1 ms triggers.
- **Outcome surfacing**: non-confirmed outcomes (`Returned`, `Transport`,
  `Backpressure`, `Closed`, per-message failures folded by the
  `publish_batch` contract) are recorded in a bounded pending-error queue
  (4096, evict-oldest + `dropped_error_records_total` counter) and raised at
  the next publish/flush/size/clear/stats operation — first record per call,
  sync-parity. New `Pool::drainErrors()` returns every record instead of
  raising; `RabbitMqQueue::drainSettlementErrors()` drains it before the
  consumer settlement check on every `pop()` (`Transport` →
  `ConnectionException`, everything else → `QueueException`).
- **Sync boundaries keep full-deadline semantics**: `flush()`, `size()`,
  `clear()`, `publishBatch`, the consumer's pre-pop buffer drain, and
  teardown (`close()`/destructor under `TEARDOWN_FLUSH_BUDGET`) quiesce
  outstanding drains first, then flush synchronously with the original
  deadline. Late drains after teardown count as `dropped_publications_total`.
- **Bounded intake**: in-flight confirm window bounded by the actor
  semaphore + byte budget (unchanged); concurrent spawned drains capped by a
  permit semaphore (8); `onBackpressure` and the surfaced drain-backpressure
  record keep backpressure observable.
- **Replay**: mid-batch connection loss replays buffered publications with
  the same `message_id` and original deadline (pre-existing actor behavior,
  pinned by a regression test in this round).

## 3. Phase 2 — official re-bench (proof)

Fresh lab cycle, release build, same 3×10 protocol, `raw/`:

| Cell | pipelined | baseline (same session) | delta |
|---|---|---|---|
| goopil-dispatch-blind | 21 939 | 21 654 | +1.3 % |
| **goopil-dispatch-safe** | **20 866** | 5 729 | **×3.64** |
| goopil-worker | 15 421 | 13 651 | +13.0 % |
| vladimir-dispatch (control) | 9 545 | 9 615 | −0.7 % |

- **Safe mode reaches blind parity**: safe/blind = 0.95 (baseline 0.26);
  both cells now sit at the produce ceiling.
- Control stable (−0.7 %) → the session comparison is fair.
- All 120 measured rounds: `ok=true`, losses=0, late_arrivals=0,
  stall_recoveries=0 — the at-least-once contract held end-to-end under the
  pipelined flush, including the recovery replay path (pinned by the
  integration chaos suite, 25 passed, and the new unroutable-publish
  surfacing test).
- Cross-session absolute deltas on dispatch cells remain a session factor
  (round-i README); the same-session ratios are the signal: safe/vladimir
  0.60× → **2.19×** (blind/vladimir 2.24× → 2.30×).

## 4. Artifacts

- `baseline/` — Phase 1 baseline session JSONs (12 runs)
- `raw/` — Phase 2 pipelined official JSONs (12 runs)
- `probe-publish-path.php` — Phase 1 path probe (same code path as the
  dispatch cell; safety-ladder + in-flight window measurements)
- `run-cell.sh` — the 3×10 cell runner
- Design note: `docs/superpowers/specs/2026-09-04-pipelined-safe-flush-design.md`
- Commits: profiling knobs `b1b6baa` (defaults = production behavior, knobs
  removed by the implementation), design note `00d84e6`, replay pin
  `30d98dc`, pipelined flush `317856b`, Laravel surfacing `12c933f`,
  design-note alignment `8330f61`, integration pin `5f79851`
