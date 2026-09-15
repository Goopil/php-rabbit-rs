# Round L re-bench — post-#302 consumer reliability fixes

Date: 2026-09-14.
Branch: `fix/consumer-backpressure-reliability` (PR #302), rebased on main at
`dff4ec3` plus #301, #303, #304. Extension: **release** cdylib
(`target/release/librabbit_rs_php.dylib`), loaded per-run with
`-d extension=...`, never installed system-wide.
PHP 8.5.6 (cli), macOS (Apple Silicon), localhost broker.

Broker: local lab, 3-node RabbitMQ 4.2.9 cluster, **fresh state**
(`./scripts/lab-down.sh -v` + `./scripts/lab-up.sh with-plugin` +
`./scripts/lab-ready.sh`), wiped again before pass 3 (see "Session incidents").

## Why this re-bench exists

PR #302 restructures the consumer actor: dedicated never-gated `ControlCommand`
channel (control-starvation fix), terminal settlement of oversized deliveries,
at-most-once early ack per delivery tag, stale-generation fencing of the poison
path, and an interaction fix with #296's deferred ack flush (`flush_acked` no
longer waits on `pending_incoming`, which cannot produce acknowledgements
before dispatch). The goal is to demonstrate **reliability without a
throughput tax**: every fixed path (manual ack via the control channel, batch
confirms, worker pop+ack) is exercised by this protocol.

## Protocol

3 interleaved passes, rabbit-rs/amqplib alternated inside each pass
(run1 rs, run2 amqplib, run3 rs, run4 amqplib). Each run: 10,000 messages
x 10 rounds + 1 warmup, 256 B payload (batch-confirm) / 1024 B (laravel-worker).
Run integrity gate: losses == 0 and duplicates == 0 on every cell — held on
all 12 runs. One rabbit-rs SKIP per fresh lab ("invalid channel state: Closing
(confirm.select)" on the first run after a wipe; the retry always succeeds) —
observed 3/3 fresh labs this session, worth a look as a warmup-race finding.

## Results (median of 3 passes, msg/s)

| Cell | rabbit-rs | amqplib | ratio |
|---|---|---|---|
| batch-confirm publish | 32,221 (30.8k–34.1k) | 43,811 (41.6k–44.2k) | 0.74x |
| batch-confirm consume | 39,359 (37.8k–42.3k) | 29,310 (28.5k–29.6k) | 1.34x |
| laravel-worker publish | 179,981 (169.4k–181.9k) | 87,379 (84.5k–87.8k) | 2.06x |
| laravel-worker consume | 17,207 (14.7k–23.3k) | 2,156 (2.0k–2.4k) | 7.98x |

The laravel-worker consume cell shows a rising trend across passes
(14.7k -> 17.2k -> 23.3k): fresh-cluster warmup, not a plateau. Reading the
ratio against that median is conservative.

## Comparison across sessions (Round 2 → Round I → this session)

| Session | Date | Code base | rs worker pub | amqplib control (worker pub) |
|---|---|---|---|---|
| Round 2 | 2026-08-31 | main `7707b5d` | 215,121 | 86,290 |
| Round I | 2026-09-03 | main `a24ef44` | 191,913 | **38,643 — control degraded, session outlier** |
| Round L (this archive) | 2026-09-14 | `dff4ec3` + #301/#303/#304 + #302 | 179,981 | 87,379 |
| Flush-cadence A/B (2026-09-15, see below) | | same build as Round L | 188,329 @1ms | 81,457 |

- **Round I is the outlier session, not this one.** Its own archive documents a
  -69%/-70% collapse on the publish-heavy driver cells for BOTH drivers
  (unchanged third-party code as the control), and its amqplib worker-publish
  control halved (86.2k → 38.6k). Its absolute values are not cross-session
  comparable; the earlier "php-amqplib moved" framing inverted the direction —
  it was Round I's amqplib cells that collapsed.
- **This session's comparator is healthy**: amqplib worker publish 87.4k ≈
  Round 2's 86.2k, worker consume 2.2k ≈ 2.2k.
- The one cell that still reads low against Round 2 is rabbit-rs worker
  publish (180.0k vs 215.1k, -16%, with a stable control). That gap triggered
  the investigation below; verdict there: session variance, not a code
  regression.
- All paths fixed by #302 show no regression in same-session data; CodSpeed
  micro-benchmarks on the PR are green.

## Investigation: worker-publish gap vs Round 2 (flush-cadence A/B)

Every commit touching the blind publish path between `7707b5d` (Round 2 base)
and this branch was audited:

- Blind byte budget (`1170da5`): 64 MiB default budget vs ≤3 MiB actually in
  flight (1024 queued + 2048 in-flight × 1 KiB worker payload) — never binds;
  its cost is two atomics per publish on a ~4.6 µs/publish budget.
- Actor mailbox coalescing (#274, `5a6a635`): blind publishes route directly
  to the pump and bypass the actor mailbox — not on this path (and it
  measured ~13% faster on its own bench anyway).
- `publisher.flush_interval` knob (5d9133b): default 1 ms reproduces the
  pre-knob behavior.
- lapin 4.10.0 → 4.11.0 (#303): upstream changelog is a single
  initial-connection-retry fix; nothing in the publish path.
- PHP-ext publish buffer: the only hot-loop-structure change is #218
  (`0de7725`, Sep 11, this-session-only): a background timer now enforces the
  1 ms age deadline that pre-#218 was only evaluated by the next publish
  call. Hypothesis: in a hot loop the timer caps every batch at ~1 ms worth
  of publications and multiplies timer + drain spawns at 180-215k msg/s.

**A/B test** (2026-09-15, fresh 3-node lab, same release cdylib as Round L,
`RABBIT_RS_BENCH_FLUSH_INTERVAL_MS` knob in the harness driver): interleaved
laravel-worker runs at the default 1 ms interval vs 100 ms, plus amqplib
controls. One cold warmup run (144k) excluded.

| Config | n | pub median (range) | con median |
|---|---|---|---|
| flush_interval = 1 ms (default) | 3 | 188,329 (175,410–188,481) | 16,474 |
| flush_interval = 100 ms | 3 | 172,760 (166,852–190,449) | 12,654 |
| amqplib control | 2 | 81,457 (81,317–81,598) | 1,903 |

**Result: #218 is exonerated.** Raising the interval 100× did not restore
Round 2 throughput (if anything slightly lower; the knob demonstrably applied
— the validated config schema is `deny_unknown_fields`, and the consume
median moved with the interval as buffered publications linger longer before
a pop drains them).

**Verdict: session variance.** Three sessions with different code states
(Round I predates #218/#274/#303 entirely) read 180–192k on this cell, while
Round 2 alone reads 215.1k with a 240.9k top run and round-level peaks at
252.3k in its raw data. Session controls drift -6..-8% vs Round 2 in the same
direction (today 81.5k; Round L batch control 43.8k vs 47.5k). Every audited
hot-path change is ns-scale against a ~4.6 µs/publish budget. Residual: a
same-session interleaved A/B of the `7707b5d` build vs this build would make
this publication-grade; not run.

## Session incidents (disclosed)

- A concurrent Orca agent session in another worktree ran
  `./scripts/test-integration.sh` on a loop, recycling the lab compose project
  mid-protocol (3 times, ~20:34 / ~20:39 / ~20:42 UTC). Passes were re-run on
  fresh labs; the 12 archived runs are the ones that completed with
  `losses == 0 && duplicates == 0`.
- Do not quote cross-session absolute deltas without the control-cell read
  from the comparison section above.

## Raw data

`raw/passN-runM-driver-scenario.json` — one JSON per interleaved run, as
emitted by `benchmarks/src/run-benchmarks.php`.
`flush-interval-ab/` — the nine JSONs of the flush-cadence A/B above
(warmup, 3×1 ms, 3×100 ms, 2 amqplib controls).
