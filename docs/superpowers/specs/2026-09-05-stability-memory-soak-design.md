# Stability & memory consolidation — soak leak proof (Round K)

Date: 2026-09-05. Tracker: #142. Companion issues: #143 (soak telemetry), #144
(nightly CI), #141 (bench error contract), #139 (flake).

## 1. Motivation

The 1.0 gate is complete (v0.1.0 shipped 2026-09-04). Memory safety is
currently proven **by construction** — publisher byte budget (64 MiB), blind
pump byte budget (#52), per-subscription consumer buffers, PHP publish buffer
ceilings (4096 msgs / 64 MiB), bounded pending-error queue (#135), lossy Drop
close fixed (#71) — but never **by measurement**. No harness samples memory;
no long-duration run exists (the Round I soak is 10 minutes, kill-focused,
with zero memory telemetry).

Goal: an irreproachable core. This round adds empirical evidence — a
long-duration soak with RSS-slope leak detection, archived locally and in a
nightly CI job — plus closes the two open rigor debts:

- **#141**: `bench.php` exits 0 without writing its output JSON on permanent
  broker failure, which silently invalidated a #140 smoke batch.
- **#139**: CI-only `PublishBufferBackpressureTest` flake (mode 2), with DIAG
  instrumentation in place waiting for data.

## 2. Scope

In scope:

- `soak.php`: memory telemetry + RSS-slope leak detection (steady + kill
  modes) — #143.
- Nightly CI soak workflow with archived JSON artifacts — #144.
- #141 fix: error JSON + non-zero exit on permanent broker failure.
- #139 bounded reproduction attempt (stop condition; DIAG-driven root cause
  if reproduced).
- Detector validation: one-off injected-leak check proving the detector fires.
- ROADMAP Round K entry + `docs/performance.md` methodology note.

Out of scope:

- Native heap profiling (dhat / malloc stats) — escalation path only if the
  RSS slope shows growth.
- Micro-optimizations (audit F-38) and `Consumer::next()` ~60 µs attribution —
  each requires a fresh post-Round-D profile first; separate round.
- F2/F3 leftovers #53 / #58 / #60 — fill capacity, not this round.

## 3. Soak memory telemetry (#143)

### 3.1 Sampling

New `--sample-interval=<s>` (default 10). At each tick the harness records:

| field | source |
|---|---|
| `t_s` | seconds since run start |
| `rss_bytes` | Linux: `/proc/self/status` `VmRSS`; macOS: `ps -o rss= -p <pid>` (pattern already used by the suite per `docs/performance.md`) |
| `php_usage_bytes` | `memory_get_usage(true)` |
| `php_peak_bytes` | `memory_get_peak_usage(true)` |
| `stats` | selected `Pool::stats()` keys: `backpressure_total`, `reconnects_total`, `dropped_publications_total`, `dropped_error_records_total`, `duplicates_total` |

**Small extension required:** the per-cycle tripwire (§3.2) needs the publish
buffer occupancy, which `stats()` does not expose today
(`PublishBuffer::buffered_len()`/`buffered_bytes()` are `pub(crate)` only).
Add two keys to `Pool::stats()`: `publish_buffered` and
`publish_buffered_bytes` (stub updated, Pest pin added). Purely additive —
existing keys unchanged.

Sampling runs alongside the existing churn loops — O(1) every 10 s, no
hot-path changes. 60 min at 6 samples/min = 360 raw samples; the JSON stores
the full series (bounded and small).

### 3.2 Leak detection

- **Warmup**: the first 20 % of the run duration is excluded from the fit
  (allocator steady state, topology declarations, runtime warm).
- **Slope**: least-squares fit over post-warmup `rss_bytes` samples,
  expressed in MB/hour.
- **Fail criterion**: slope > `--leak-mb-per-hour` (default 20). Rationale:
  bounded buffers oscillate; a genuine leak (per-cycle retained Arc/map
  growth) shows monotonic growth. 20 MB/h is loud on a multi-GB headroom yet
  tolerant of allocator/GC noise. Flag-tunable; the first archived runs
  calibrate the default.
- **Per-cycle tripwire**: `publish_buffered == 0` (new `stats()` key, §3.1)
  at the end of every cycle. The flush-until-success loop already guarantees
  this today (`flush()` quiesces spawned drains then sync-drains, so a
  successful flush leaves the buffer empty); a violation catches a re-buffer
  leak path.
- **Reported, not asserted**: RSS return-to-baseline after `close()`
  (allocator retention makes it noisy) and PHP peak memory.

### 3.3 Modes

- **Kill mode** (default, `--kill-every=10`): semantics unchanged — proves
  recovery + at-least-once under churn.
- **Steady mode** (`--kill-every=0`, already supported): sustained pop+ack,
  cleanest leak signal.
- Exit 0 only when: `missing == 0`, (kill mode) `reconnects ≥ 1`, no stall,
  per-cycle buffered == 0, RSS slope ≤ threshold.

### 3.4 JSON contract

The result JSON gains a `memory` block:

```json
{
  "memory": {
    "sample_interval_s": 10,
    "warmup_s": 720,
    "threshold_mb_per_hour": 20,
    "rss_slope_mb_per_hour": 1.4,
    "rss_before_bytes": 48234496,
    "rss_after_close_bytes": 47861760,
    "php_peak_bytes": 8388608,
    "samples": [{"t_s": 0, "rss_bytes": 48234496, "php_usage_bytes": 2097152, "php_peak_bytes": 2097152, "stats": {}}]
  }
}
```

The J1 metric-contract fields (config/meta blocks) are already present in the
harness and unchanged.

## 4. #141 — bench.php error contract

On permanent broker failure (warmup/purge), `bench.php` must:

1. write the `--output` JSON with the standard envelope plus an error payload:
   `{"benchmark": ..., "ok": false, "error": {"kind": "broker_unavailable",
   "message": ...}}`;
2. exit non-zero (exit 2, consistent with the existing bad-argument exit
   code).

Callers (`scripts/rebench-driver-bench.sh`, `run-cell.sh`) already fail on
non-zero exit — verify, do not rework. Contract restored: "exit code 0 only
when no message was lost, and only ever with the output JSON written."

## 5. #139 — bounded reproduction attempt

The flake reproduces only in the CI Coverage (PHP Extension) job: the full
PHP Pest suite in one process, ~94 µs/iteration vs 12–36 µs locally.

Procedure (bounded):

1. Local reproduction loop: run the full ext Pest suite N times (budget: 20
   full-suite runs) under CPU contention, watching for the mode-2 null
   refusal and capturing the DIAG lines (`DIAG PublishBufferBackpressureTest`
   sightings histogram + `stats()` snapshot).
2. If reproduced: DIAG data → root cause → fix → remove the diagnostic
   instrumentation (kept on purpose for exactly this).
3. If not reproduced after the budget: stop, document in #139 (runs
   attempted, environment, result), keep the diagnostic and the issue open.
   The nightly soak adds an independent stability signal meanwhile.

Non-goal: weakening or skipping the test.

## 6. Nightly CI workflow (#144)

`.github/workflows/soak.yml`:

- Triggers: `schedule` (nightly cron, UTC) + `workflow_dispatch` with a
  `minutes` input.
- Steps: `rust-setup` composite action (from #140) → build release cdylib →
  `./scripts/lab-up.sh with-plugin` + `lab-ready` → two segments with the
  extension loaded via `-d extension=<release artifact>`:
  1. **steady** (30 min, `--kill-every=0`) — leak evidence;
  2. **kill** (15 min, `--kill-every=10`) — recovery evidence.
- Both JSONs uploaded as workflow artifacts; the job fails if any segment
  exits non-zero.
- Runner: Linux (glibc x86_64), PHP + composer setup mirroring the Laravel
  integration job.
- Known GH behavior: scheduled workflows are disabled after 60 days of repo
  inactivity — acceptable, noted in a workflow comment.

## 7. Docs

- ROADMAP: Round K section (motivation, scope, success criteria) + execution
  queue update.
- `docs/performance.md`: soak/memory methodology subsection (sampling,
  warmup, slope threshold, modes, where evidence is archived).

## 8. Success criteria

1. `soak.php` emits the `memory` block and fails on: RSS slope > threshold
   after warmup, per-cycle buffered ≠ 0, missing > 0, stall, or (kill mode)
   no reconnect.
2. Detector validated: an injected leak (one-off temporary patch, ~50 MB/h
   equivalent) makes the soak fail on slope; patch reverted.
3. Local evidence archived under `benchmarks/results/round-k-soak/` (60-min
   kill run + 30-min steady run, README summary — round-d archive pattern).
4. Nightly CI green on main with archived artifacts.
5. #141 closed: `bench.php` never exits 0 without writing its output JSON;
   callers fail loudly.
6. #139 fixed (DIAG removed) or the bounded investigation documented with a
   stop condition.
7. Quality gate `./scripts/check.sh` green; no hot-path changes.

## 9. Risks

- **Threshold calibration**: too tight → false positives on allocator noise;
  too loose → missed slow leaks. Default 20 MB/h, flag-tunable, calibrated by
  the first archived runs.
- **RSS measurement noise** (macOS `ps` granularity): the slope over 40+
  samples is robust to single-sample jitter.
- **Nightly cost**: ~45–60 min of runner time per day.
- **Perturbation**: sampling is O(1) every 10 s; telemetry does not touch the
  churn loops, so measured invariants (throughput, stall detection) are
  unaffected.
