# Round K — stability & memory soak evidence (issues #142/#143/#144)

Date: 2026-09-05. Design: `docs/superpowers/specs/2026-09-05-stability-memory-soak-design.md`.
Extension: **release** cdylib (`target/release/librabbit_rs_php.dylib`,
rabbit_rs 0.1.0), loaded per-run with `-d extension=...`, never installed
system-wide. PHP 8.5.6 (cli), macOS (Apple Silicon), local lab (3-node
RabbitMQ + toxiproxy, `./scripts/lab-up.sh with-plugin`).

Harness: `benchmarks/driver-bench/bin/soak.php` (Round I kill machinery +
Round K memory telemetry). Both runs use `--fill=1000`,
`--sample-interval=10`; the sequence tracker is the compact bitset
(`bitMarkReceived`/`bitPopcount`), so harness memory stays flat and cannot
contaminate the RSS signal.

| Run | Command | ok | exit | missing | duplicates | reconnects | tripwire | raw slope | envelope slope |
|---|---|---|---|---|---|---|---|---|---|
| steady 30 min | `--minutes=30 --kill-every=0` | true | 0 | 0 | 1 | 0 | 0 | -13.3 MB/h | **+0.6 MB/h** |
| kill 60 min | `--minutes=60 --kill-every=10 --kill-timeout-ms=50` | false* | 1* | 0 | 17 655 (0.6 %) | 297/297 kills | 0 | +63.3 MB/h | **0.0 MB/h** |

(*) see "Detector calibration" below — the embedded `ok/slope` fields of the
kill run were produced by the pre-calibration raw least-squares fit; the
calibrated peak-envelope estimator applied to the same archived samples
reports 0.0 MB/h and passes.

## Verdict

- **Steady (leak evidence)**: 5.9 M messages pop+ack over 30 min, zero
  loss, one duplicate, RSS plateau after warmup — leak-free.
- **Kill (recovery evidence)**: 2.9 M messages across 297 deterministic
  both-legs connection kills, `missing = 0` (at-least-once holds),
  duplicates counted and redelivered, publish-buffer tripwire never fired
  (buffer quiesces to 0 after every cycle's flush), RSS bounded.

## Detector calibration (found by the kill run)

The 60-min kill run's raw RSS series oscillates between two bounded
allocator states (~25 MB and ~90 MB) with deep transient dips — macOS
libmalloc returns a large arena wholesale, then the process re-grows into
it. A raw least-squares fit over that series misreads the uneven plateau
durations as growth (+63.3 MB/h) although the run ends at 25.5 MB, far
below its 96 MB maximum: **a false positive, not a leak**.

Calibration delivered in the same round:

1. The estimator now fits a **peak envelope seeded by the warmup peak**
   (`rssSlopeMbPerHour` in `benchmarks/driver-bench/bin/soak_memory.php`):
   a genuine leak must exceed the warmup peak and keep climbing, which
   raises the envelope; bounded oscillation keeps it flat.
2. Unit tests cover the sawtooth shape (returns 0.0), the linear leak, and
   a leak climbing past the warmup peak.
3. Re-validation with an injected leak: PHP huge blocks (> 2 MB, mmap'd —
   arena-invisible small chunks do not move RSS) retained at ~3 MB per
   sample make the soak fail with a 7 576 MB/h fitted slope; patch reverted.

To reproduce the envelope verdict from the archived samples:

```bash
php -r 'require "benchmarks/driver-bench/bin/soak_memory.php";
$j = json_decode(file_get_contents("benchmarks/results/round-k-soak/kill-60min.json"), true);
var_dump(rssSlopeMbPerHour($j["memory"]["samples"], 720.0));' # 0.0
```

## Files

- `steady-30min.json` / `.stderr.txt` — steady segment result + progress trail.
- `kill-60min.json` / `.stderr.txt` — kill segment result + progress trail.
- The nightly CI soak (`#144`, `.github/workflows/soak.yml`) archives its own
  artifacts as `soak-results` and was validated end-to-end via
  `workflow_dispatch` (steady 2 min + kill 2 min, run 33986997296, success).
