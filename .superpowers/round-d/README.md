# Round D Phase 1 — safe publish path root cause (issue #41)

Date: 2026-09-04. Worktree `task/41-round-d` @ `5df69ba` (+ experiment commit `b1b6baa`).
Lab: fresh 3-node RabbitMQ 4.2.9 (`lab-down` → `lab-up with-plugin` → `lab-ready`).
Extension: release cdylib built in the worktree, loaded via `-d extension=`, never installed.
Probe: `.superpowers/round-d/probe-publish-path.php` (same code path as the dispatch cell:
`Queue::push` → `Pool::publish` → `PublishBuffer` → `publish_batch`).

## 1. Baseline (same session, official 3×10 protocol, round-median)

| Cell | rate | round-i archive | note |
|---|---|---|---|
| goopil-dispatch-blind | 21 699 | 21 992 | matches |
| **goopil-dispatch-safe** | **5 740** | 6 534 | the target |
| goopil-worker | 14 245 | 16 234 | |
| vladimir-dispatch (control) | 9 624 | 9 685 | matches → session comparable |
| vladimir-worker | 1 899 | 2 041 | |

Safety ladder (probe, same session): blind 21 036 → unsafe 17 782 → safe 5 445 msg/s.
Safe-mode confirm RTT (actor-perceived, `confirmation_latency`): p50=3 ms, p95=4, p99=6–9.

## 2. In-flight window finding — the serial-await hypothesis, corrected form

**Refuted as stated:** safe mode does NOT await one confirm per message (that would cap at
1/1.8 ms ≈ 550 msg/s; measured 5.3–6k). Within one flush batch, `publish_batch` releases all
messages (`try_publish`) before awaiting outcomes (`PublishWaiter::wait_all`) — the Transport
API is used as designed.

**Confirmed in batch form:** the PHP-side `PublishBuffer` flush (`block_on(publish_batch)`)
serializes each batch's confirm-wave against production. Measured with `RS_PROF`:

- Batch size is window-limited: interval 1 ms × intrinsic PHP produce rate (~24–25k/s) →
  **batch p50 = 24–25 msgs**.
- `block_on` per flush: **T_flush ≈ 2.0 ms + 36 µs × N** (fits N=25/48/64/1024:
  3.0 ms / 4.4 ms / 4.4 ms / 32.2 ms per batch; per-msg 123 → 37.4 µs).
  - N=1 flush = **1.8 ms** (pure single-confirm RTT; broker group-commit on Docker volume).
  - 36 µs/msg marginal = actor + confirm machinery + cross-thread wake chain (unsafe mode
    measures 5.2 µs/msg for the same path minus confirms).
- At baseline, block_on = **74 % of wall time**; `sample` shows main thread 63 % parked in
  `_pthread_cond_wait` (waiting for confirms) while the tokio worker is 97 % in `kevent` and
  lapin-io-loop 97.5 % in `semaphore_wait` — **latency-bound, not CPU-bound**.
- Confirms arrive asynchronously at p50=3 ms latency but ~21k/s sustained (probe with the
  barrier removed, below) — the pipeline overlaps RTTs fine; the barrier is what serializes.

## 3. Experiments

| Experiment | result |
|---|---|
| (a) tokio workers 1→4 (`RS_TOKIO_WORKERS`) | **no delta**: safe 4881→4290, blind 21 036→21 857 (noise). Threads are idle; CPU is not the constraint. |
| (b) deeper window (`RS_FLUSH_MS` 1→2→4→0, `RS_FLUSH_MAX` 64→256→1024) | 6.0k → 7.3k → 8.9k/9.0k → **12.1k** (threshold-bound batches of 1024). Follows the T_flush model; saturates at the barrier+produce asymptote (~13k). |
| (b2) **barrier removal** (`RS_ASYNC_FLUSH=1`: flush spawns on the runtime, outcomes discarded — contract-violating prototype) | safe dispatch cell **5.3k → 21.5k median (×4.05)**, official 3×10 protocol, ok=1 in all runs; equals blind/produce ceiling (21.0–21.9k). |
| (c) property-clone dedupe | header-amplification probe was confounded (conversion + wire cost mixed in). Bound: total non-wait CPU on the path is ~5 µs/msg (unsafe marginal), so double property clones ≤ a few µs/msg — **not a lever at 15–25k rates**. |

## 4. Ranked root causes (measured contribution)

1. **block_on flush barrier** (`PublishBuffer::flush_batch` → `client.publish_batch` awaited
   synchronously by the PHP thread): 74 % of wall time at baseline. Removing it: **×4.05**
   (5.3k → 21.5k). THE root cause.
2. **Small in-flight window** (1 ms interval → ~24-msg batches): costs ~40 % on top of (1)
   (6.0k → 12.1k when deepened). Subsumed by (1): with async flush the default window already
   reaches the produce ceiling.
3. **Single tokio worker** (`runtime.rs`): measured dead end (×1.0).
4. **Property double-clones / per-message allocs**: bounded small (≤ ~5 µs/msg of the 36 µs
   marginal; the rest is confirm machinery + wake chains). Only matters if rates >25k/s are
   ever needed — measured dead end for this round.
5. **~2 ms single-confirm RTT**: broker-side group-commit latency on this lab; NOT a client
   defect. Overlaps away under (1); amqplib parity (transport laravel-dispatch 20.3k/s)
   confirms the broker sustains ≥20k/s confirmed.

## 5. Recommended Phase 2 scope (smallest set with biggest measured leverage)

**Pipelined safe flush**: make the PHP-buffer flush non-blocking (spawn on the runtime, as
`RS_ASYNC_FLUSH` prices) while restoring the contract the prototype dropped:

- bounded intake (existing 4096 msgs / 64 MB budget) + backpressure when the drain falls
  behind (semaphore or capacity check on the spawned drain),
- surface `Returned`/terminal outcomes asynchronously (EventBridge callback pattern already
  exists) instead of raising them from `publish()`,
- quiesce on `flush()`/`close()`/destructor under `TEARDOWN_FLUSH_BUDGET` (count unconfirmed
  as `dropped_publications_total`, keep at-least-once semantics),
- keep replay/bounded-buffer invariants (actor-side `publish_batch` already replays with the
  same `message_id` and original deadline).

Expected: goopil-dispatch-safe 5.7k → ~21k (parity with blind and ~1.06× the amqplib
laravel-dispatch transport reference), confirm-latency percentiles unchanged.

**Measured dead ends (do NOT do):** more tokio workers; per-message alloc/clone surgery;
deeper flush windows as the primary fix (bounded ×2 and still 3.3× short of parity);
actor/transport rewrites — the transport layer sustains 21k+ with threads idle.

## 6. Artifacts

- Baseline JSONs: `.superpowers/round-d/baseline/` (5 cells × 3 runs)
- Verify runs: `.superpowers/round-d/verify-current/`, `verify-async/`
- Probe + runner: `probe-publish-path.php`, `run-cell.sh`
- Experiment commit: `b1b6baa` (env-gated knobs; defaults = production behavior)
