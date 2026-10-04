# Round L profile — fresh post-audit profile and optimization decision gate

Date: 2026-10-04. Branch `fix/audit-2026-10-01` at `936c062`, tree clean of commits
(one uncommitted bench-config patch, reverted before committing — see
"Dead bench config" below). Phase 2 gate had passed; the branch is stable for
measurement. **This task produces measurements and decisions, not production
code.** Exactly one candidate graduates to an implementation task (appended to
the plan); four are rejected with numbers.

## Machine and environment context (read before comparing numbers)

- MacBook Pro (Apple Silicon), macOS 26.6.2, PHP 8.5.6 (cli, NTS),
  ext-rabbit_rs **0.3.10 release cdylib** (`target/release/librabbit_rs_php.dylib`,
  built after `936c062`, sources verified not newer than the artifact), loaded
  per-run with `-d extension=...`, never installed system-wide.
- Broker: local lab, fresh 3-node RabbitMQ 4.2.9 cluster
  (`./scripts/lab-down.sh && ./scripts/lab-up.sh && ./scripts/lab-ready.sh`
  before the runs), Toxiproxy + Prometheus up, delayed-message plugin enabled.
- **The machine was not quiet.** Background load averages ranged 5.7–13.1
  during the session (the user's RustRover IDE indexing this very repo at
  >100 % CPU, PhpStorm, a VM, backup agents). This load is outside the
  measurement's control. Mitigations, and why the numbers below are still
  decision-grade: interleaved pass ordering, medians over 3+ runs, paired A/B
  runs taken back-to-back under identical load, and percentile statistics
  (p50/p95) instead of means where the tail dominates. Per-run load
  observations: `profiles/load-observations.csv`; session provenance:
  `profiles/provenance.json`.
- **Xdebug-free invocation:** this machine's default `php.ini` loads xdebug
  (plus pcov, openswoole, opentelemetry, protobuf, redis). Every measured
  process ran under `php -n -d extension=<dylib>` — no ini files at all — and
  the runner asserted `extension_loaded('xdebug') === false` per session
  (`tools/provenance.sh`). Laravel bootstrap needs only compiled-in modules,
  which `php -n` retains.
- Micro-tool versions: `profiles/meta` of every JSON records php/rabbit_rs/os.

## Method

Three measurement layers, chosen for per-stage attribution without new global
tooling (`samply` is not installed; `/usr/bin/xctrace` and `/usr/bin/sample`
are):

1. **Driver-bench standard protocol** (`benchmarks/driver-bench/bin/bench.php`,
   Laravel queue API, 1024 B envelope, `--count=1000 --rounds=10`): 3 cells
   (dispatch-blind, dispatch-safe, worker=pop+ack) × 3 interleaved passes,
   one JSON per pass under `raw/`.
2. **Micro matrix** (`tools/micro-publish.php`, `tools/micro-consume.php`,
   `tools/fill.php` — purpose-built harnesses that bootstrap the driver-bench
   app only to reuse its compiled native config; the measured sections call
   Pool/Consumer/Delivery directly): single-run publish ladder
   (safe/blind/unsafe × 50k), 3-run consume stages, and a 3×3 paired A/B
   (`--props=minimal|full|hdr1`) for the property-conversion candidates.
3. **System profiles** (xctrace Time Profiler, `--launch`, 1 ms sampling,
   exported via `xctrace export --xpath .../table[@schema="time-profile"]` and
   aggregated by `tools/xctrace-tree.py` into per-symbol self/inclusive call
   trees, archived under `raw/traces/`): publish-safe 200k publishes,
   consume-hot 100k deliveries, consume-earlyack 100k deliveries, plus a
   bootstrap-only run (`--iters=1`) used to subtract Laravel startup from
   main-thread CPU. Rust symbols resolve from the release dylib. A fourth
   recording under the Allocations template **hung and was abandoned**;
   allocation evidence comes from the trace allocator-frame self-time plus the
   paired A/B instead.
4. **Rust micro-benches** (divan via `cargo bench -p rabbit-rs-core --features
   bench`, release bench profile): scheduler pick scaling, delivery round
   trip, publisher pump. Note: the workspace uses `codspeed-divan-compat`
   5.0.1, which reports walltime only (no divan alloc columns).

Caveats stated once and applied everywhere: xctrace *self* time is exact; its
exported *inclusive* shares are lower bounds (stack-depth truncation in the
export); divan timings and micro p50s carry the background-load noise
described above (reported as medians).

## Salvage, quarantine, and two environment catches

The prior (interrupted) attempt left `profiles/` and `raw/` JSONs with **no
provenance metadata** (no xdebug flag, empty stderr) on a machine whose default
PHP loads xdebug. Its numbers could not be verified clean, so **everything was
re-run**; the prior artifacts are preserved untrusted under
`profiles/prior-attempt-unverified/` and `raw/prior-attempt-unverified/`
(including that implementer's own `xdebug-contaminated` quarantine — the
discipline was right, the release/xdebug verification was missing). Re-running
produced numbers consistent in shape with the prior pass, which additionally
supports the prior implementer's tool design.

Two environment defects were caught and fixed for these runs:

1. **Stale vendored package.** `benchmarks/driver-bench` resolves
   `goopil/rabbit-rs-laravel` via a path repository with `symlink: false`;
   the vendored copy had drifted from `packages/laravel-queue/src` (verified by
   `diff -rq`). Refreshed (`composer update goopil/rabbit-rs-laravel
   --ignore-platform-req=ext-amqp`), now byte-identical. All numbers below run
   the current package.
2. **`micro-consume.php` iters-default bug.** `max(1, (int)($args['iters'] ??
   0))` coerced the unset default to 1, defeating the stage fallback
   (`$iters ?: min($fill, 20_000)`), collapsing hot/batch/earlyack to
   single-sample distributions. Fixed in the tool (`iters` defaults to 0 and
   lets each stage apply its own fallback); `hot_try` additionally primes the
   buffer with blocking `next()` calls because `tryNext()` never blocks and 50
   instant nulls would end the loop before the pump delivers.

## Dead bench config (hygiene finding, verified, reverted)

`benchmarks/driver-bench/config/rabbit-rs.php` shipped a nested
`'topology'` block that **nothing reads**. `ConnectionCompiler::topology()`
(`packages/laravel-queue/src/Config/ConnectionCompiler.php:653-658`) reads only
the flat top-level keys `queue_type`/`queue_durable`/`delivery_limit`/
`dead_letter` (default `quorum`); `CONNECTION_KEYS` (`:45-56`) has no
`topology` entry. The nested block survives `rejectUnknownKeys` (`:81`) only
because the same dead object is passed as `$defaults`
(`RabbitMqConnector.php:47` → `RabbitRsConnections::packageDefaults()` reads
`config('rabbit-rs')`), whose keys are whitelisted. The package default
(`config/rabbit-rs.php:47`) is `queue_type => 'quorum'`, so the committed bench
config would silently bench **quorum** queues while the other two drivers use
classic — an unfair comparison.

The prior implementer's uncommitted patch added the flat
`'queue_type' => 'classic'` key with a revert note; the claim **verified true**
against the compiler source. The patch was kept for every measurement in this
archive and **reverted before committing** (the archive was taken with it
applied — recorded here so the numbers are interpretable). A future hygiene
task should replace the dead nested block in the committed bench config with
the flat key.

## Driver-bench matrix (standard protocol)

3 interleaved passes, medians below; per-pass JSONs in `raw/passN-*.json`.
All passes: `ok=1`, `losses=0`, `late_arrivals_after_drain=0`,
`reconnects_total=0` — delivery contract intact at 65–72k pub/s and 22–33k
pop+ack/s.

| Cell            | rate ops/s (median) | p50    | p95    | p99    |
|-----------------|--------------------:|--------|--------|--------|
| dispatch-blind  | 71 380              | 13 µs  | 17 µs  | 24 µs  |
| dispatch-safe   | 65 753              | 13 µs  | 17 µs  | 25 µs  |
| worker (pop+ack)| 23 495              | 7 µs   | 25 µs  | 281 µs |

Pass spread: dispatch-blind 71 284–71 834 (±0.4 %); dispatch-safe
65 125–68 142 (±2 %); worker 22 154–33 447 (pass2 is a background-noise
outlier; the median is reported).

## Micro matrix

Publish ladder, 50k single publishes per run (`profiles/publish-*.json`):

| Mode   | p50      | p95     | p99     | mean     | rate ops/s |
|--------|---------:|---------|---------|----------|-----------:|
| safe   | 1.167 µs | 2.584 µs| 7.917 µs| 15.001 µs | 66 663     |
| blind  | 1.000 µs | 1.833 µs| 5.709 µs| 10.432 µs | 95 856     |
| unsafe | 1.041 µs | 1.917 µs| 5.125 µs| 8.432 µs  | 118 597    |

The safe→unsafe p50 gap is 0.13 µs; the *mean* gap (~6.6 µs) is buffered-flush
tail, not per-op confirm cost — confirms the confirm waiter is amortized
off the per-publish path (`confirmation_latency_us_p50` = 7 µs broker-side).

Consume stages (`profiles/consume-*.json`; hot = next(1000)+ack on a filled
backlog, prefetch 64 unless noted):

| Stage                  | p50         | p95        | p99         | mean        | runs |
|------------------------|------------:|------------|-------------|-------------|-----:|
| tryNext empty (FFI floor) | 0.125 µs | 0.167 µs   | 0.167 µs    | 0.123 µs    | 2    |
| next(0) empty (block_on slow path) | 1 175 µs | 1 271 µs | 1 949 µs | 1 176 µs | 2 |
| hot fill1000           | 0.75–0.875 µs | 110–148 µs | 355–516 µs | 21.3–22.5 µs | 3 |
| hot fill1000 prefetch500 | 0.834 µs | 128 µs     | 431 µs      | 22.8 µs     | 1 |
| hot_try (warm buffer, tryNext) | 0.167–0.208 µs | 0.25–0.29 µs | 1.2–1.9 µs | 0.19–0.25 µs | 2 |
| earlyack fill1000      | 1.125–1.584 µs | 49–63 µs  | 188–257 µs  | 11.0–11.9 µs | 3 |
| earlyack fill5000      | 1.208 µs  | 54 µs       | 175 µs      | 11.4 µs     | 1 |
| batch fill5000         | 19.9 µs/item (avg batch 5.3, 50 269 items/s) |||| 1 |

Two structural readings:

- **`next()` p50 is a buffered pop (sub-µs); the mean is tail-dominated.**
  Hot p50 0.75–0.875 µs vs mean 21–22.5 µs: the p95/p99 refill stalls (pump
  hand-off + socket) dominate the mean. Prefetch 64 vs 500 moves the tail,
  not the p50.
- **`next(0)` on an empty queue costs ~1.2 ms/call** — `block_on(timeout(0))`
  still pays the runtime park/unpark cycle (`classes/consumer.rs:349-364`),
  while `tryNext()` stays on the lock-free fast path at 0.125 µs. Poll-shape
  workers should prefer `tryNext`/`block_for` over `next(0)` spinning.

Rust-side divan benches (release bench profile, medians). The raw console
output of the Task-17 session benches is archived verbatim — the scheduler
bench was re-run during review with the same result (values in the file
headers; session vs re-run spread ≤ ~12 % at the noisiest cell, verdict
unchanged): `raw/divan-scheduler.txt`, `raw/divan-consumer-delivery.txt`,
`raw/divan-publisher.txt`.

| Bench                          | Result (session; re-run in the archived file header) | Per-unit        |
|--------------------------------|------------------------------------------------------|-----------------|
| weighted_fair_round, 4 subs    | 193.5 ns / round of 4 picks (re-run 185.8 ns)        | ≈48 ns/pick     |
| weighted_fair_round, 32 subs   | 9.083 µs / round of 32 picks (re-run 9.124 µs)       | ≈284 ns/pick    |
| delivery_ack_round_trip        | 2.874 µs (re-run 3.041 µs)                           | per delivery    |
| delivery_ack_burst(64)         | 179.3 µs (re-run 184 µs)                             | ≈2.8 µs/delivery|
| pump_batch_128                 | 104.2 µs (re-run 127.3 µs)                           | ≈0.81–1.0 µs/publish |

## Publish safe path — end-to-end per-stage breakdown

Trace `raw/traces/publish-safe-*` (200k publishes, 2 847 samples; on-CPU only;
1 ms sampling). Bootstrap subtraction: the `--iters=1` control
(`raw/traces/bootstrap-*`) shows the Laravel/bootstrap cost on the main thread
is ≈152 ms of CPU; the measured loop therefore accounts for ≈415 of the main
thread's 567 on-CPU samples ≈ **2.1 µs main-thread CPU per publish**.

| Stage (thread)                      | Evidence                                   | Per-publish            | Confidence |
|-------------------------------------|--------------------------------------------|------------------------|------------|
| PHP loop + FFI + conversion (main)  | FFI handler `do_try_catch` 50.3 % incl.; `conversion::publish` 38.8 % incl. (lower bound); `reject_unknown_keys` 8.5 %; `Pool::publish` 8.1 % | ≈2.1 µs CPU total; conversion ≈ half of it | HIGH (self) / MEDIUM (split) |
| Budget location formatting (main)   | `core::fmt::Write::write_str` self 0.5 % (3/567 samples over 200k) | ≈15 ns                 | HIGH (small) |
| Buffer enqueue + pump hand-off (actor) | `publish_queue` 2.3 % incl. visible; `PublishBuffer::flush_pipelined` task 15.5 % | inside 5.0 µs actor CPU | MEDIUM |
| Confirm waiter (actor)              | `ConfirmationResult` FuturesUnordered poll 17.8 % incl.; `resolve_confirmation` self 0.9 % | ≈0.89 µs of 5.0 µs actor CPU | HIGH (share) |
| Wire write (io thread)              | `lapin Buffer::write` self ≈5 %; `io_loop` self 5.4 % | ≈1.67 µs io CPU total; write ≈0.08 µs | HIGH (self) |
| Allocations (all threads)           | allocator-family self: actor 13.6 %, main 6.0 %, io 7.8 % | ≈0.68 µs actor + ≈0.13 µs main | HIGH (self) |

Actor-thread CPU is 5.0 µs/publish (1 000 samples / 200k) — the pipeline is
CPU-parallel across main/actor/io threads, so wall-clock per-op p50 (1.17 µs)
is lower than any single thread's CPU share.

## `Consumer::next()` attribution — resolving the ~60 µs open question

The ROADMAP figure (2026-09-01 smoke benchmark) was **~60 µs per unit
`next()` at the Laravel `pop()` layer**, capping unit-consume at 16–22k jobs/s
while `nextBatch` sustained 41k+/s. Today's measurements decompose it:

| Layer                                             | Measured today                                        |
|---------------------------------------------------|-------------------------------------------------------|
| Laravel worker `pop()`+ack+job hydration (driver-bench worker) | p50 7 µs, p95 25 µs, p99 281 µs          |
| Extension boundary `next(1000)`+`ack()` on hot backlog (micro) | p50 0.75–0.875 µs, mean 21–22.5 µs        |
| Extension boundary `tryNext()` empty (FFI floor)  | p50 0.125 µs                                          |
| Rust-side delivery→dispatch→ack round trip (divan, mock transport; `raw/divan-consumer-delivery.txt`) | 2.87 µs                              |

Per-stage attribution for `Consumer::next()`:

| Stage                                        | Number                                            | Confidence |
|----------------------------------------------|---------------------------------------------------|------------|
| FFI boundary + guards (lock-free fast path)  | 0.125 µs (`tryNext` empty, 200k iters × 2)        | HIGH       |
| Buffered pop + Delivery→PHP conversion + ack enqueue | hot p50 0.75–0.875 µs (minus the 0.125 floor ≈ 0.6–0.75 µs of pop+conversion+ack) | HIGH combined / MEDIUM internal split |
| Pump hand-off (actor dispatch, flume)        | `dispatch` 16.2 % incl. of 3.77 µs/delivery actor CPU ≈ 0.61 µs | MEDIUM     |
| Socket read (io thread)                      | 2.97 µs/delivery io CPU (297 samples/100k), allocator 5.1 % | MEDIUM (share) |
| Refill tail (waits, not CPU)                 | p95 110–148 µs, p99 355–516 µs — dominates the mean | HIGH       |
| Confirm waiter                               | publish-only stage; see publish table             | n/a        |
| Empty slow path `next(0)`                    | ≈1 175 µs/call (`block_on` park/unpark floor)     | HIGH (measured) / MEDIUM (mechanism) |

**Verdict on the 60 µs question:** the per-call extension cost is *not* 60 µs
— it is sub-µs at p50 with a refill-dominated mean (~22 µs). The historical
60 µs was the full Laravel-layer unit-pop cost (queue API, envelope JSON
decode, Job construction, per-job ack crossing) on older code; the current
Laravel worker p50 is 7 µs. The batched-`pop()` idea in ROADMAP's parked list
remains valid precisely because of the per-crossing floor (0.125 µs) plus
Laravel per-job overhead, not because `next()` itself is slow. No core
fast-path work is warranted by this profile.

## Decision gate — the 5 audit candidates

Verdicts: **KEEP** requires a measured number and a projected gain worth a
task; **REJECT** records the measured cost that killed it; nothing is decided
on intuition.

| # | Candidate | Measured cost | Projected gain | Verdict |
|---|-----------|---------------|----------------|---------|
| 1 | Scheduler per-pick `Vec` + O(n²) `contains` (`scheduler.rs:96-124`) | ≈48 ns/pick @4 subs → ≈284 ns/pick @32 subs (divan medians, archived in `raw/divan-scheduler.txt` with a matching re-run; sub-quadratic in practice: several O(n) passes); driver-bench workers use 1 subscription where the pick is off the hot path | <0.5 % of even the sub-µs buffered pop; ≤0.3 % actor CPU at 32 subs | **REJECT** (283.8 ns/pick @32) |
| 2 | One `tokio::spawn` per delivery on the early-ack path (`actor.rs:441-446`) | Paired A/B at fill=1000: earlyack p50 1.125–1.584 µs vs hot 0.75–0.875 µs → **+0.5–0.65 µs handoff p50**; trace: `dispatch` CPU delta ≈ +0.11 µs/delivery; early-ack actor CPU 2.52 vs hot 3.77 µs/delivery (early-ack is overall cheaper — no PHP ack crossing) | Batching acks would save ≤0.5 µs/delivery **only** in the opt-in early-ack mode and changes settlement semantics | **REJECT** (+0.6 µs p50, +0.11 µs CPU per delivery) |
| 3 | 2 `String` allocations per message for `MessageId` (`actor.rs:390-398, 495`) | Total allocator-family self-time on the consume actor thread = 13.8 % of 3.77 µs/delivery ≈ **0.52 µs/delivery for ALL Rust allocations**; `MessageId`'s 2 clones (`MessageId(String)`, `delivery.rs:21`) are 2 of ~6–10 per-message allocs → ≈0.1–0.15 µs/delivery | ≈≤0.7 % of the Laravel per-message cost (p50 7 µs worker / 22 µs micro mean) | **REJECT** (0.52 µs/delivery total allocator self-time; MessageId is a small subset) |
| 4 | 2–4 per-publish `String` allocations (`delay.rs:142-158` + `lapin.rs:665-693`) | Paired A/B (safe, 50k, 3 runs each): content_type absent vs present p50 deltas **+0.33/+1.08/+0.21 µs** (median +0.33, mean +0.54 µs); the always-on `message_id` double-clone is present in *both* arms, so this undercounts; allocator family ≈0.8 µs/publish across threads; `route_transport_request` runs per publish on both paths (`pump.rs:269`, `actor.rs:769`) and `publish_properties` clones every string again | Eliminating the redundant `to_owned`→`clone` chain (share one `Arc<str>`/borrow between the transport request and `BasicProperties`) saves ≈0.3–0.6 µs/publish p50 ≈ 25–40 % of the extension-boundary publish p50, and removes actor-thread allocator pressure | **KEEP** → task appended to the plan |
| 5 | Eager budget-location formatting (`conversion.rs:55-75, 392, 446`) | `write_str` self = 3/567 main-thread samples over 200k publishes ≈ **15 ns/publish** (≈1.3 % of p50); the 1-header A/B delta is unresolvable at this machine's noise floor (median-of-3 +0.08 µs, single runs ±0.7 µs) | <2 % of the publish p50; the formatted strings carry error-path value only | **REJECT** (15 ns/publish) |

### Graduated task

Candidate 4 graduates. Appended to
`docs/superpowers/plans/2026-10-01-post-audit-stabilization.md` as
**Task 24: Eliminate duplicate per-publish property string clones**. The
implementing task must re-run the props A/B (`tools/micro-publish.php
--props=minimal|full`) as its regression gate: the goal is to move the
`full` envelope's p50 toward the `minimal` floor (1.2 µs) without changing
wire behavior.

## Method discipline notes

- **Contamination lesson (recorded per the dispatch):** a profile archive is
  only as good as its provenance. Every measured JSON in this archive records
  php/rabbit_rs/os in `meta`; the session-level `profiles/provenance.json`
  records the xdebug-free assertion and load averages; `profiles/load-observations.csv`
  brackets every cell with 1/5/15-minute load. Any future re-run should keep
  the same three artifacts or explain their absence.
- **Instrument quirks encountered:** the xctrace Allocations template hangs
  headless on this machine (abandoned, CPU trees + A/B used instead);
  `codspeed-divan-compat` has no alloc columns; xctrace `--output` must
  precede `--launch`; xctrace export needs absolute binary paths in `--launch`.
- **No production code was changed.** The diff contains only this archive,
  the reverted bench-config patch is out, and the only plan edit is the
  appended Task 24.

## Reproducing

```sh
# environment
./scripts/lab-down.sh && ./scripts/lab-up.sh && ./scripts/lab-ready.sh
rtk cargo build -p rabbit-rs-php --release
cd benchmarks/driver-bench && composer update goopil/rabbit-rs-laravel \
  --ignore-platform-req=ext-amqp && cd ../..

# driver-bench (xdebug-free, per cell × 3 interleaved passes)
cd benchmarks/driver-bench
RABBIT_RS_SAFETY=safe php -n -d extension=../../target/release/librabbit_rs_php.dylib \
  bin/bench.php --connection=rabbit-rs --mode=dispatch --count=1000 --rounds=10 --output=...

# micro matrix (medians over ≥3 runs; keep the machine as quiet as possible)
../results/round-l-profile/tools/micro-matrix.sh

# traces (Time Profiler, 1 ms sampling; export + aggregate)
xctrace record --template 'Time Profiler' --output <name>.trace --time-limit 30s \
  --launch -- "$(which php)" -n -d extension=<dylib> <tool> --iters=200000
xctrace export --input <name>.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > <name>.xml
python3 ../results/round-l-profile/tools/xctrace-tree.py <name>.xml
```
