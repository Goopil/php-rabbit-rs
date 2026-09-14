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

## Comparison with Round I (2026-09-03)

Round I ratios: batch-confirm 1.03x publish / 4.3x consume; laravel-worker
4.9x / 6.4x. Two session-level shifts prevent a like-for-like ratio read:

1. **php-amqplib moved**: its batch-confirm consume went 9.5k (Round I) ->
   29.3k (this session) and its publish 31k -> 43.8k. When the comparator
   driver shifts that much between sessions, cross-session ratio deltas are
   session factors, not code signals (benchmarks/README.md framing).
2. **rabbit-rs worker cells track Round I closely** on absolute values:
   publish 180k vs 191.9k (-6%, single node of a 3-node fresh cluster),
   consume 17.2k vs 12.1k (+42%, warmup trend). The fixed paths show no
   regression on same-protocol cells.

The honest conclusion: **no throughput regression from the #302 reliability
work is visible in same-session data**; CodSpeed micro-benchmarks on the PR
are green as well. For a publication-grade ratio table, re-run both drivers
in one session on the post-#305 main once the current wave lands.

## Session incidents (disclosed)

- A concurrent Orca agent session in another worktree ran
  `./scripts/test-integration.sh` on a loop, recycling the lab compose project
  mid-protocol (3 times, ~20:34 / ~20:39 / ~20:42 UTC). Passes were re-run on
  fresh labs; the 12 archived runs are the ones that completed with
  `losses == 0 && duplicates == 0`.
- Absolute numbers differ from Round I more than usual because of the
  comparator shift above; do not quote cross-session deltas.

## Raw data

`raw/passN-runM-driver-scenario.json` — one JSON per interleaved run, as
emitted by `benchmarks/src/run-benchmarks.php`.
