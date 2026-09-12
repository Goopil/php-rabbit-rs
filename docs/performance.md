# Performance methodology and RC budgets

How performance is measured in this repository, what the release-candidate (RC)
budget gate enforces, and when re-baselining is legitimate. The machine-readable
source of truth for the baseline is `benchmarks/baselines/reference-machine.json`;
this document explains how it is produced and how to read it.

## Measurement methodology

The same rules as the release protocol in `benchmarks/README.md` apply to
every recorded measurement:

- **Release build mandatory.** The extension is benchmarked from the release
  cdylib (`cargo build --release -p rabbit-rs-php`,
  `target/release/librabbit_rs_php.dylib`), loaded per-run with
  `-d extension=...` and never installed system-wide. A debug build masks
  throughput by ~4×.
- **Interleaved runs.** `./scripts/rebench-driver-bench.sh` alternates cells
  (goopil/vladimir × dispatch/worker) across passes instead of completing one
  side first, so drift (cache, thermal, broker state) hits both sides equally.
- **0 losses / 0 duplicates expected.** Where the workload operates in safe
  mode (confirms + mandatory), the delivery contract is at-least-once with
  measurable duplicates; a benchmark run with a non-zero loss/`missing` or
  duplicate counter is invalid as a measurement and is rejected by the budget
  checker's always-blocking rules.
- **Lab, not the wild.** The 3-node RabbitMQ lab + toxiproxy
  (`./scripts/lab-up.sh with-plugin`, verified by `./scripts/lab-ready.sh`)
  provides the broker; quorum queues, prefetch 64, ~1024 B Laravel envelope.
- **Latency is per-op**, measured by `bench.php` with `hrtime()` around each
  `Queue::push` (dispatch cells) or pop+ack call (worker cell); end-to-end
  publish→consume latency is a separate transport-suite metric and is not part
  of the RC budget.

## The four budget metrics

| Metric | Source cell(s) | JSON field |
|--------|----------------|------------|
| publish throughput | `goopil-dispatch-safe`, `goopil-dispatch-blind` | `avg_rate_ops_s` |
| consume throughput | `goopil-worker` | `avg_rate_ops_s` |
| publish p99 | `goopil-dispatch-safe`, `goopil-dispatch-blind` | `latency_ms.p99` |
| consume p99 | `goopil-worker` | `latency_ms.p99` |

## Thresholds and their rationale

| Check | Budget | Rationale |
|-------|--------|-----------|
| publish throughput | ≥ 80 % of baseline | Clean runs sit within ~±10 % of the median (see the variance note in the baseline JSON); a 20 % allowance absorbs normal variance while a genuine regression (typically > 2×) trips far past it. |
| consume throughput | ≥ 80 % of baseline | Same rationale on the drain path. |
| publish p99 | ≤ 150 % of baseline | Per-op p99 is noisier than the mean; 50 % headroom tolerates tail jitter while still catching tail regressions (a doubled p99 fails). |
| consume p99 | ≤ 150 % of baseline | Same rationale on the drain path. |
| `ok`, losses/`missing`, duplicates, final `publish_buffered`, buffered tripwire | always blocking | These are delivery-contract integrity, not performance: silent loss is unacceptable, duplicates must stay zero for these workloads, and a non-zero `publish_buffered` final reading means publications parked across cycles (a re-buffer leak path). No threshold can buy back a violation. |

Two deliberate design choices in `check-budgets.php`:

- **Median-of-runs per scenario.** Transient noise occasionally halves a single
  run's throughput (observed on this machine: mid-run dips of 30–55 % in 2 of
  3 rabbit-rs cells, recovering within the same run). Comparing each file
  separately would cry wolf on noise; comparing the median of the runs in the
  invocation keeps the gate stable while a real regression — which shifts
  every run — still fails.
- **`n/a` never passes.** A null metric means "not measured for this run"
  (e.g. `duplicates` in dispatch cells, where nothing is consumed) and an
  unknown scenario means "no baseline"; neither is allowed to count as a
  passing check, and an invocation where no check could be applied fails.

Exit codes: `0` all applied checks pass, `1` budget fail or unjudgeable data
(unrecognized schema, unparseable JSON), `2` usage error.

## Reference machine

The baseline records the machine it was actually measured on — spec, PHP and
driver versions, broker version, git SHA and date are in
`reference-machine.json` (`meta` and `coverage` blocks). The current baseline
was measured on an Apple M2 Max (12 cores, 32 GB, macOS 26.6.2/arm64), PHP
8.5.6, RabbitMQ 4.2.9 lab. Do not quote the baseline as a portable number:
it is a same-machine regression reference, not a marketing figure.

Honesty rule: a baseline commit must never contain numbers the machine did not
produce. If a cell could not be measured, record the gap in `coverage.notes`
instead of estimating it.

## Re-baselining policy

Re-baselining replaces the reference numbers every future RC run is judged
against, so it is **explicit, dated, and never silent**.

When it is legitimate:

- The reference environment changed: hardware, OS/macOS version, Docker or
  broker version, PHP version, or a driver major version.
- An intentional, reviewed performance trade-off landed (profiled, with the
  numbers in the change description) and the old baseline would now fail every
  honest run.

When it is **not** legitimate: making a red gate green without explaining why
the numbers moved; re-running until a favorable median appears and keeping only
that run; extending coverage with numbers from a different machine.

Procedure:

1. Start the lab (`./scripts/lab-up.sh with-plugin`) and confirm readiness
   (`./scripts/lab-ready.sh`); build the release extension.
2. `./scripts/rebench-driver-bench.sh benchmarks/results/<dir>` — keep the
   archived JSONs outside the repo (or copy them out) as raw provenance.
3. Regenerate `benchmarks/baselines/reference-machine.json`: per-cell median of
   the passes, real machine spec, PHP/driver versions, git SHA, date, coverage
   notes (including any variance observations).
4. Verify the new baseline passes its own runs:
   `php benchmarks/baselines/check-budgets.php benchmarks/results/<dir>`.
5. Commit the baseline JSON with the reason in the message; the commit is the
   dated record. If the change is a trade-off, link the profiling evidence.
