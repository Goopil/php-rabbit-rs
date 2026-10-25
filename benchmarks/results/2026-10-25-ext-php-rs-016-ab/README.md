# ext-php-rs 0.15.15 vs 0.16.3 — same-driver A/B (throughput and memory)

Date: 2026-10-25. Question: did the ext-php-rs **0.16.3 migration** (commit
`63e5800`, callbacks/bridge rework + module lifecycle; shipped as
`rabbit_rs-php 0.3.12`) gain, cost, or change memory behavior vs the last
**0.15.15** state (`159e45b`, `rabbit_rs-php 0.3.11`)?

Method: the issue-368 comparison harness (`benchmarks/src/run-comparison.php`,
loaded per-run with `-d extension=<path>`) driven once per build with an
identical command — only the loaded `.dylib` changes. Both cdylibs are release
builds of their own commit (0.15.15 built in a throwaway worktree with that
commit's `Cargo.lock`). Pinned environment: [`env-pin.json`](env-pin.json).

## Verdict table (median of 5 measured rounds per cell)

Throughput (consume rate, msg/s) — deltas relative to 0.15.15:

| Scenario | 0.15.15 (0.3.11) | 0.16.3 (0.3.12) | Δ throughput |
|---|---|---|---|
| `sustained-consume` (50k, push, manual ACK, prefetch 128) | 25,327 | 27,120 | **+1,793 msg/s (+7.1%)** |
| `laravel-worker` (10k, pop + ACK, prefetch 64) | 16,144 | 18,049 | **+1,905 msg/s (+11.8%)** |

Memory (PHP worker process, median over the 5 invocations per cell; KB):

| Metric | Scenario | 0.15.15 | 0.16.3 | Δ RSS |
|---|---|---|---|---|
| Peak RSS (`/usr/bin/time -l` max RSS) | sustained-consume | 115,312 | 115,200 | −112 (−0.1%) |
| Peak RSS (`/usr/bin/time -l` max RSS) | laravel-worker | 65,792 | 66,112 | +320 (+0.5%) |
| Steady-state RSS (median of 2nd-half samples) | sustained-consume | 106,784 | 103,344 | −3,440 (−3.2%) |
| Steady-state RSS (median of 2nd-half samples) | laravel-worker | 55,880 | 57,680 | +1,800 (+3.2%) |

## Verdict (2 lines)

- **Throughput: 0.16.3 gained — +7.1% (sustained-consume) and +11.8%
  (laravel-worker) at the medians.** Round-paired (interleaved cycles), 0.16.3
  won 4/5 rounds on sustained-consume and 3/5 on laravel-worker; the median
  gain is consistent across both scenarios, the per-round spread is wide
  (rates in the raw JSONs: sustained-consume 22.7k–27.8k vs 24.2k–32.8k;
  laravel-worker 12.5k–25.8k vs 13.8k–21.6k).
- **Memory: unchanged behavior — peaks within ±0.5% and steady-state within
  ±3.2%, moving in opposite directions across the two scenarios (run noise);
  no regression and no gain from the 0.16 migration.**

Negative results stated as-is: the steady-state deltas (+3.2% / −3.2%) are
opposite-signed across scenarios and are within this host's run-to-run spread;
we do not claim a memory improvement or a memory cost. Round-paired wins of
3/5 mean the laravel-worker throughput gain is directionally supported but not
uniform per round — quote the medians, not a per-round guarantee.

## Protocol

- 5 interleaved cycles per scenario, 0.15.15 → 0.16.3 back-to-back inside each
  cycle (drift-sharing alternation, same technique as the issue-368 archive).
  Cycle 1 included one warmup round per build (`--warmup=1`), excluded from
  medians → 5 measured rounds per build per scenario.
- Scenario configs identical to the 0.3.12 archive (issue-368):
  `sustained-consume` = blind batched fill (unmeasured) → push consume,
  manual ACK per message, prefetch 128, 50,000 msgs × 1024 B;
  `laravel-worker` = blind fill → dedicated consumer pop + ACK, prefetch 64,
  10,000 msgs × 1024 B. Durable classic queue, persistent delivery.
- Integrity gates held everywhere: **0 losses, 0 duplicates on all 20
  measured rounds** (10 per build).
- Memory measured per invocation: `ps` sampling of the PHP process every
  100 ms + `/usr/bin/time -l` maximum resident set size (macOS reports bytes;
  converted to KB). Every invocation is one PHP process running exactly one
  round (cycle 1: warmup + measured round in the same process, both builds
  treated identically).
- Broker: fresh 3-node lab (`./scripts/lab-up.sh with-plugin`) started for
  this session, RabbitMQ 4.2.9 / Erlang 27.3.4.16, same pinned image as the
  issue-368 archive. toxiproxy failed to bind (unrelated ssh tunnel holds
  ports 24500–24509) — not used by these scenarios, same as issue-368.

## Reproduction (one command per cell)

```bash
./scripts/lab-up.sh with-plugin && ./scripts/lab-ready.sh   # broker
AB=benchmarks/results/2026-10-25-ext-php-rs-016-ab
# 0.15.15 cells (worktree built with: git worktree add --detach
#   /Users/zacharyvolpi/dev/perso/rabbit-rs-ab015 159e45b
#   && cargo build -p rabbit-rs-php --release   # in the worktree)
$AB/rss-run.sh /Users/zacharyvolpi/dev/perso/rabbit-rs-ab015/target/release/librabbit_rs_php.dylib sustained-consume $AB/raw-0.15.15 0
$AB/rss-run.sh /Users/zacharyvolpi/dev/perso/rabbit-rs-ab015/target/release/librabbit_rs_php.dylib laravel-worker   $AB/raw-0.15.15 0
# 0.16.3 cells (release cdylib of main)
$AB/rss-run.sh target/release/librabbit_rs_php.dylib sustained-consume $AB/raw-0.16.3 0
$AB/rss-run.sh target/release/librabbit_rs_php.dylib laravel-worker   $AB/raw-0.16.3 0
```

Each `rss-run.sh <dylib> <scenario> <outdir> <warmup>` is one harness
invocation (append to `<outdir>/partial-rabbit-rs.json`) plus RSS sampling
(`<outdir>/rss-<scenario>-<ts>.samples.txt`) and
`/usr/bin/time -l` (`<outdir>/time-<scenario>-<ts>.log`). Summaries:
`php $AB/rss-summarize.php <outdir> <label>` (writes `rss-metrics.json`) and
`php benchmarks/src/run-comparison.php --merge --output-dir=<outdir>`
(writes `comparison-results.json`, integrity gates inside). Re-running a cell
appends new runs to the partial; the archived files are this session's runs.

## Files

- `raw-0.15.15/`, `raw-0.16.3/` — per build: `partial-rabbit-rs.json`
  (every run, warmups included), `comparison-results.json` (merged envelope),
  `rss-metrics.json` (memory summary + per-invocation detail),
  `rss-*.samples.txt` + `time-*.log` + `run-*.log` (raw evidence).
- `verdict.json` — the verdict table as data (medians and deltas, computed
  from the raw files, no hand-typed numbers).
- `env-pin.json` — pinned environment: commit hashes, build commands,
  ext-php-rs pins and lock versions, extension versions (verified live:
  `phpversion('rabbit_rs')` = 0.3.11 / 0.3.12), broker image digests, PHP,
  hardware.
- `rss-run.sh`, `rss-summarize.php` — the measurement wrapper and summary
  (archive-local; the issue-368 harness itself was not modified).

Every number in the verdict table appears in
`raw-0.15.15/comparison-results.json` / `raw-0.16.3/comparison-results.json`
(throughput) and `raw-0.15.15/rss-metrics.json` /
`raw-0.16.3/rss-metrics.json` (memory); `verdict.json` restates the table
itself as data.
