# Roadmap

Running trace of upcoming work. When a round starts it gets a full dated plan in
`docs/plans/`; this file keeps the queue, motivation, scope, and success criteria so
nothing is lost between rounds.

Ground rule: every user-facing feature ships with a runnable usage example in
its matching docs page as part of its issue's success criteria — examples land
with the feature, never in a later docs pass.

## Landed

- Native design + implementation plans (2026-07-30) — milestone details live there.
- Fair benchmark comparison (2026-08-16) and benchmark fixes (2026-08-21).
- Publisher batcher removal (2026-08-21) and publish pump v2 (2026-08-28).
- Post-pump simplification (2026-08-29, plan `2026-08-29-post-pump-simplification.md`):
  Phase A realistic Laravel benchmarks, Phase B dead-code sweep, Phase C mimalloc
  (evaluated, rejected on data), Phase D publisher.safety plumbing + invariant tests +
  multi-broker consumer fan-in, Phase E driver-level benchmark vs amqplib/amqp-ext
  (merged as PR #35, 2026-08-30 — includes the local ext-amqp cross-check and the
  publish safety variant matrix).
- Round 2 (plan `2026-08-30-consumer-stall-and-reliability.md`): P1/P2/P3 resolved on
  main (`bbd836b`, `08ba5e8`); secondary scope + re-bench done 2026-08-31
  (branch `task/39-secondary-rebench`, archive
  `benchmarks/results/round-2-rebench/`).
- **Release v0.0.8 (2026-08-31)** — first release exercising the new delivery
  pipeline end to end: unified `-nts` naming, blocking `verify-pie-install`
  (real `pie install` against the published release; PIE 1.4.10 cannot resolve
  draft releases, so the job runs after publish and gates Homebrew + the
  Laravel split; the installer itself runs as root to bypass the interactive
  sudo check), ZTS dropped from the matrix (16 → 8 assets). Contains the 17
  production-readiness issues closed on 2026-08-31 (Round 2 fixes, publish
  buffer bound, consumer deadline, Horizon after-commit, ClearableQueue,
  lazy establishment, events bridge, TLS loud failures).
- **Release v0.0.9 (2026-09-02)** — ships all of Round G waves 1–6 (transport
  liveness P0, poison terminal settlement, DLQ bindings, delay validation,
  delay-queue identity + GC, config surface, extension boundary correctness,
  log facade, duplicates metric, size/clear truthfulness, blind byte budget,
  batch error contract, declare-before-consume) plus promoted P1s #56/#52 and
  follow-ups #95/#96/#50. Pipeline lesson: `verify-release` needed
  `contents:write` — draft releases are invisible to read-only tokens, so the
  first v0.0.9 run failed with "release not found"; tag re-pointed and the
  full pipeline (10 builds, verify, publish, PIE install, Laravel split,
  Homebrew) then went green.
- **Technical audit (2026-08-31)** — `docs/audits/2026-08-31-technical-audit.md`:
  7 passes (architecture, 5 parallel adversarial passes, red team, per-finding
  verification), 40 consolidated findings, every cited location re-verified at
  `b06b62f`. Headline: connection-loss detection has no production trigger
  (P0), CI never runs a real-broker test, plus a family of
  suspension-without-wake-up and silent-loss paths. Findings split into
  Round G below.
- **Round G wave 1 (2026-09-01)** — merged via PRs #92/#93/#94:
  - #66 (P0): transport liveness — `TransportErrorStream` selected in the
    Ready loop, consumer delivery-stream termination surfaces a terminal
    error, 10s connect timeout, `recovery_failures_total`; lab kill-broker
    test proves auto-recovery (24/24 integration).
  - #70 (P1): poison deliveries settle terminally (DLX or documented ack),
    `consumer.max_attempts` configurable, attempts never fabricated,
    unmarshable payloads settled; Laravel `consumers.max_attempts` plumbed.
  - Round C (#40): local Laravel integration harness green (3 consecutive
    runs; 14 passed + 3 loud toxiproxy skips); 8 failure classes root-caused.
  - Sonar note: automatic analysis ignores `sonar.cpd.exclusions` — test
    duplication counts against the ≤3% new-code gate; keep test code
    deduplicated (shared helpers live in `tests/Pest.php`).
  - Follow-ups filed: #95 (consume/declare race on fresh quorum queues),
    #96 (PublishBuffer first-publish flush), #97 (delayed bindings +
    mandatory/safe-mode interaction).
- **Round G wave 2 (2026-09-01)**:
  - #73 (P1): delays validated against the compiled strategy — a TTL-mode
    publish whose delay exceeds the largest bucket is refused terminally
    (`PublishErrorKind::InvalidRequest`) before any transport operation
    instead of executing immediately on the original exchange; the
    delayed-release refusal settles the original delivery terminally
    (`ConsumerErrorKind::InvalidDelay`, DLX or documented ack) instead of
    hot-looping; the extension conversion rejects the delay at the boundary
    naming the limit; plugin mode unchanged.
- **Round G waves 3–4 (2026-09-01)** — merged via PRs #103–#110:
  - #68 (P1): workers evict cached consumers on source replacement and close
    (the three `ConnectionException` kinds and `Closed`); lab rejoin test on a
    dedicated vhost.
  - #74 (P1): supervisor exit 0 = clean exit (budget reset + immediate
    restart, no duration heuristic); crash-loop budget intact for non-zero
    exits; four tests that encoded the bug migrated to crash mode.
  - #77 (P2): admin operations routed through the connection actor
    (`open_admin_channel` reuses the publisher channel path); the per-process
    connections registry was removed — one connection per vhost is now
    structural, not a convention.
  - #75 (P2): extension event callbacks — thrown PHP exceptions preserved and
    rethrown after the drain (later callbacks still fire), single-slot theft
    fixed by a multi-callback registry + `Pool::clearEventCallbacks(): int`,
    drain hoisted onto the `next()`/`tryNext()`/`nextBatch()` fast paths.
  - #78 (P2): config surface enforced — `mandatory: false` rejected with a
    `publisher.safety` migration pointer (honoring it would confirm
    unroutable publishes = silent loss), `confirm_timeout >= 1s`,
    `heartbeat` bounded 1..65535 s, `mandatory` dropped from the pool
    fingerprint (validated constant).
  - #80 (P2): Laravel config validation deferred off `boot()` (lazy
    `rabbit-rs.config` singleton at connection resolution), env-string
    booleans accepted via `filter_var`, Octane reload re-normalizes.
  - #89 (P1): release pipeline — Packagist token via Bearer header from a
    0600 temp file + `--fail` + body assertion; mirror auth via GIT_ASKPASS
    (no token in URLs/argv); mirror force-pushes removed (fast-forward only,
    published tags immutable); explicit job permissions.
- **Round G wave 5 (2026-09-01)** — merged via PRs #113–#117:
  - #56 (promoted P1): log facade — `Sink` trait (Info ⊂ Warn ⊂ Error),
    first-wins install, silent by default; stderr sink gated by the
    `RABBIT_RS_LOG` env var at MINIT (no PHP-callable sink: callbacks cannot
    cross Tokio threads); `CoordinatorError` typed enum replaces string
    errors; `recovery_coordinator` panics removed (dead watch → `Closed`
    error); panic audit in `docs/reliability.md`.
  - #79 (P2): delay queue identity — synthesized delay queues embed an
    8-hex args digest (SHA-256 over bucket ms, expiry margin, DLX, DLRK) so
    rolling config changes open fresh queues instead of 406-storming;
    `sweep_delay_queues()` GC with conservative gates (quorum queues reject
    `if-unused`/`if-empty` on RabbitMQ 4.2).
  - #76 (P2): extension boundary correctness — key validation no longer
    debug-only (release builds reject typo'd options), `ackBatch` > 256
    pre-check, Array/Table headers round-trip as nested PHP arrays (core
    gains a `Decimal` `HeaderValue` variant + once-per-process precision
    notice), pool teardown flush budget 500ms (was blocking up to 30s on a
    dead broker) with a `dropped_publications_total` stat, Laravel
    `registerDefaultCallbacks` clears stale callbacks first.
  - #90 (P1): release pipeline pinning — multi-arch PHP build images
    digest-pinned, PIE 1.4.10 pinned + sha256-verified before attestation
    check, matrix cells fail loudly, `curl --proto "=https"` on the phar
    download (Sonar flagged the redirect loophole).
  - #88 (P1): chaos suite pinned to the lab — lab ships a digest-pinned
    toxiproxy (port 18474) with fingerprint proxies; the suite fails loudly
    on a foreign/dead instance, runs each scenario through a private proxy
    (unique name, 24504–24509 range, deleted in teardown), kills both proxy
    legs (a single-leg `reset_peer` leaves sockets half-open), and covers
    9 scenarios (TCP reset before confirm / before ACK, leader shutdown,
    node restart, consumer partition, channel topology error, delay plugin
    down, bad credentials, SIGTERM with unacked work).
  - Sonar note: the duplication gate (≤3% new code) bit the chaos PR at
    5.0% — resolved with a `managementRequest()` helper and pattern reuse;
    the wave-1 rule "keep test code deduplicated" stands.
- **Round G wave 6 (2026-09-01)** — merged via PRs #118–#123, six parallel
  file-disjoint tracks:
  - #87 (P1, red-team AUDIT-031): `Pool::size()`/`clear()` flush the publish
    buffer first — the first ≤63 publications of a fresh pool no longer sit
    in process memory (`size()` returned 0; `clear()` purged then the
    buffered publications repopulated the queue).
  - #96 (P3): the publish buffer arms its flush deadline on the first
    publication of a batch, so small batches are time-flushed by age instead
    of waiting for the size threshold. The chaos suite had to pin wire state
    explicitly (`flush()` before disruptions): a buffered publication left
    the proxied connection idle (reset_peer never fired) or was dropped by
    the `recreatePool` teardown.
  - #52 (P1, red-team AUDIT-023): blind publishes reserve payload bytes
    against the shared byte budget (RAII guard released when the job leaves
    the pump); over-budget streams reject with `Backpressure` instead of
    growing memory. Fire-and-forget A/B benchmark: 228.5k → 232.7k pub/s.
  - #83 (P2, audit F-15): `publish_batch` resolves publications already
    accepted by an actor before returning the first terminal failure
    (documented contract; blind mode keeps immediate failure).
  - #95 (P2): the topology reconciler is shared between recovery and
    on-demand consumer establishment — declare-before-subscribe is enforced,
    a fresh quorum queue can no longer 404 the `basic.consume` and burn a
    recovery generation (lab: 25 fresh-queue acquisitions, 0 reconnects).
  - #50 (P2): duplicates are measurable — redelivered-flagged deliveries
    count into `duplicates_total` (`MetricsSnapshot` + `Pool::stats()`).
- **Release v0.0.9 (2026-09-02)** — tagged `898764e` after the
  verify-release draft-visibility fix; full pipeline green (10 builds,
  attestation verify, publish, PIE install end-to-end, Laravel split,
  Homebrew formula).
- **External review triage (2026-09-02)** — independent review of the
  project (GO decision): the consumer advantage is real (4–6×), the safe
  publish path is the throughput ceiling, and the benchmarks should stop
  working around the consumer. Directive: **stop features → fix consumer →
  optimize confirms → prove results → ship 1.0.** Triaged into Round I
  (#126, P0), Round D promoted to P0 pre-1.0 (#41), Round J (#127–#129,
  P1); feature freeze until the P0s land.
- **Round G exit + Round H config redesign (2026-09-02)** — PRs #124/#125
  (#97: delayed-exchange bindings declared in declare topology mode,
  delayed publishes never mandatory at wire level — safe mode keeps
  confirms; #81: `rabbit-rs:status` queue stats from the management API
  behind per-connection `management_url`, native metrics honestly labeled
  same-process only) and **v0.1.0 config redesign on main via PR #130**
  (connection-first `queue.connections.*`, `ConnectionCompiler` replaces
  `ConfigNormalizer`, lazy per-connection compile — F-27/F-28 closed by
  construction, work-command fan-out with by-definition `--queue`
  resolution, subscriptions escape hatch, CHANGELOG v0.1.0 migration table;
  89 compiler unit cases, Laravel 330 Unit+Feature, Integration 24/24 on
  the lab, Rust untouched; Sonar gate lesson: `new_duplicated_lines_density`
  counts 10+-line duplicated blocks, not S1192 literals — test fixtures
  deduplicated 99 → 11 lines). Spec §5 amended to the delivered mechanism
  (one `queue:work` child per connection). Tag v0.1.0 not yet cut.

## Round I — consumer correctness under stress (P0, external review 2026-09-02)

Motivation: external review (see Landed) — the benchmark suite still works
around the consumer, and issue #111 (flaky 30 s consumer-ready timeout,
~1 in 4 extension Pest runs) is open. P0: no new features until this round
is green. Tracker: #126.

Where we already are: the ack-pipeline stall was root-caused and fixed in
Round 2 (#36/#43, settlement-error channel non-blocking; re-bench
`stall_recoveries = 0` in 30/30 worker rounds), pre-fill missing deliveries
fixed (#37), declare-before-consume enforced (#95). `benchmarks/driver-bench/bin/bench.php`
still carries the workarounds the review flagged (400-consecutive-null
stall detection + connection rebuild, consumer-created-after-fill
ordering).

Scope:

1. **#111**: reproduce (20-run loop), root-cause the suspected
   RuntimeRegistry cross-test pollution (which entry leaks, from which
   test), fix, 20 consecutive clean runs.
2. **Verify the stall fix under stress** — chaos suite + long-running soak
   (sustained pop+ack worker). If a stall reappears, root-cause it; never
   re-patch a benchmark workaround.
3. **Reconnect correctness proof**: kill the broker mid-consume →
   auto-recovery → resume; assert ACKs are generation-aware (stale ACKs
   rejected, redelivery happens); extend the chaos suite where a scenario
   is missing.
4. **No silent loss**: 0-loss invariant asserted on every scenario.
5. **Remove the benchmark workarounds** from `driver-bench/bin/bench.php`:
   delete the silent stall-rebuild and the consumer-ordering dance; keep
   loud failure detection only (fail the run instead of silently
   recovering).
6. Regression + stress tests (Rust integration + PHP) for every root cause
   fixed.

Definition of Done (long-running worker): connect → consume → ack → lose
RabbitMQ → reconnect → resume — no manual intervention, no silent loss.

Success criteria: #111 closed with 20 clean runs; benchmark workarounds
deleted (runs fail loudly instead); soak + chaos green; quality gate
`./scripts/check.sh` green.

## Round J — proof and packaging (P1, external review 2026-09-02)

Motivation: external review — turn the benchmarks into product proof and
make installation boring before 1.0. Three file-disjoint tracks, trackers
#127–#129.

### J1 — benchmark proof (#127)

Every published result set (all suites, all archived JSON) surfaces
throughput, p50/p95/p99, losses, duplicates, reconnects, stall recoveries,
safety mode, and RabbitMQ + PHP configuration (add reconnects where
missing). Framing becomes workload-scoped — "on this workload, with this
configuration and these guarantees, rabbit-rs reaches X msg/s against Y" —
with driver semantics/configuration differences explicit; no absolute
"N× faster" claims. Delivery semantics documented next to the numbers:
at-least-once (duplicates possible and measured), replay buffer is
process-memory only (not durable across a PHP crash — external outbox for
cross-process durability).

### J2 — installation matrix (#128)

Validate PHP 8.4 + 8.5 (NTS), Linux x86_64 + ARM64 (glibc + musl), macOS
ARM64, Laravel 12 + 13, and PIE install/upgrade/rollback. The release
pipeline already digest-pins images and blocks on `verify-pie-install`
(#90, v0.0.9); extend with musl + ARM64 + Laravel 13 + upgrade/rollback.
Coordinate with #58 (composer-level install friction) — do not duplicate.

### J3 — product positioning (#129)

README rewrite around: "Native Rust RabbitMQ transport for high-throughput,
long-running PHP/Laravel workers." Benchmarks front and center; safety
ladder shown as a trade-off (blind ~76.8k → unsafe ~62.5k → safe ~9.8k
msg/s); limitations documented (duplicates, in-memory replay buffer,
confirmed-publish ceiling pending Round D).

Success criteria: archived result sets carry the full metric set;
comparison docs use workload-scoped claims; install matrix green or
explicitly best-effort with loud failures; README leads with the consumer
advantage and documents limitations.

## Next — Round 2: consumer stall and reliability

Full plan: `docs/plans/2026-08-30-consumer-stall-and-reliability.md`.

Found by the Phase E driver-level benchmark; all three defects reproduce on demand and
none is a delivery loss (messages stay `ready` in RabbitMQ; at-least-once holds).

1. **P1 — stall of the ack pipeline under sustained pop+ack.** The consumer stops
   receiving while messages stay ready (observed with prefetch 1 and 64). The bench
   runner detects it (400 consecutive null pops) and rebuilds the connection;
   ~0.6–0.8 s per stall is billed into measured time (`stall_recoveries` reported).
   First task: root-cause investigation (prior exploration came back empty).
   **First hypothesis to test (from the 2026-08-30 production-readiness audit):** the
   per-broker consumer actor blocks on a *blocking* send to the bounded
   settlement-error channel — 11 `state.error_tx.send(...)` sites in
   `crates/rabbit-rs-core/src/consumer/actor.rs` on a `flume::bounded(256)` channel
   (`consumer/set.rs`). If the embedder never calls `drain_errors()`, 256 settlement
   errors freeze the actor loop: deliveries stop flowing while messages stay `ready`
   — the exact observed symptom. The fix (drop-oldest wrapper, ~30 lines) is
   Round F Task 1; apply it before deeper root-cause work and re-run the Phase E
   reproduction to confirm or eliminate it.
2. **P2 — pre-fill missing deliveries (~2%).** If the native consumer is created
   before the fill has been ingested (first `pop()` while the fill is in flight, or a
   consumer left idle across rounds), a fraction of messages never surfaces on that
   connection. Verified: consumer created pre-fill → ~2% missed; created after the
   fill → clean. Other drivers (amqplib, amqp-ext, bunny) are unaffected.
3. **P3 — `Pool::clear()` with a pre-existing consumer.** Combo degrades pops ~25×.
   Needs a dedicated core-level test.
   **Resolved 2026-08-30 (no core defect):** the dedicated core tests
   (`crates/rabbit-rs-core/tests/pool_clear.rs`) show the combination is safe —
   deliveries keep flowing, generations and settlements are untouched, and no
   re-establishment or connection storm exists. The observed ~25× is the P2
   configuration (consumer attached while the next round's fill is ingested,
   plus the 404→pop-drain fallback) amplified by the matrix's 1 s null-pop
   block; see the P3 section of the round plan for the full mechanism and the
   documented `clear()` → consumer sequence.

Secondary scope (all DONE 2026-08-31):

4. **Test the closed-pump batch contract** (`client.rs`): pinned with a
   deterministic test-support closed-pump construction; mutation-checked.
5. **`scripts/lib-extension.sh` rebuild-on-change**: DONE — staleness from
   source/manifest mtimes plus a feature stamp (detects artifacts rebuilt
   without `--features extension-tests` by other cargo commands).
6. **Parked minors rolled in**: symmetric `flush_blind` non-vacuous assert
   (D2 style, mutation-checked), shellcheck clean on
   `scripts/test-integration.sh`, subscription-name uniqueness validation
   (typed error on the exact config path).

Then: **re-bench** with the Phase E-style interleaved protocol — DONE
2026-08-31, archived in `benchmarks/results/round-2-rebench/`. Worker goopil
median 21 747 ops/s vs the 10 030 taxed baseline (+117 %), `stall_recoveries = 0`
in 30/30 worker rounds, 0 losses / 0 late everywhere; non-confirm cells match
their E2 references within session drift (goopil blind 70 262 vs 76 794; vladimir
dispatch 32 193 vs 31 182).

Success criteria: each bug root-caused and fixed (or its ceiling documented with
data), full quality gate green, re-bench archived and compared.

## Round F — production readiness hardening (audit 2026-08-30)

Motivation: full-stack production-readiness audit of the ecosystem (core Rust, PHP
extension, Laravel package) on 2026-08-30. Detailed TDD plan with exact files, test
code and verification commands:
`docs/superpowers/plans/2026-08-30-production-readiness.md` (tasks 1–20). Split into
two waves around Round 2 and the re-bench.

### F1 — correctness & safety blockers (runs alongside Round 2)

1. **Consumer actor: make the settlement-error channel non-blocking** (drop-oldest,
   plan Task 1) — also Round 2 P1 hypothesis #1; fix first, then re-test the stall
   reproduction.
2. **Horizon: honor `after_commit` and wire `bulk()`** (plan Task 5) — the Horizon
   subclass bypasses `enqueueUsing`, so transactional jobs publish before the SQL
   commit (job loss on rollback); `bulk()` skips `JobPayload::prepare()` so bulk jobs
   are invisible in the dashboard. Prerequisite: base `prepareBatch`/`publishBatch`
   go from `private` to `protected`.
3. **Bound the extension publish buffer** (plan Task 2) — `publish_buffer`
   (`crates/rabbit-rs-php/src/classes/pool.rs:46`) grows unbounded during outages;
   cap at 4096 messages / 64 MiB with explicit `BackpressureException` (already
   accepted messages are never dropped). Includes a benchmark non-regression step.
4. **Consumer acquisition deadline** (plan Task 3) — `ClientPool::consumer()`
   (`client.rs:330+`) loops forever against a black-holed broker and freezes FPM
   workers; add `consumer.wait_timeout` (default 30 s, validated 1 s–24 h), typed
   transport error at expiry, Laravel `consumers.wait_timeout`.
5. **TLS: fail loudly on unreadable certificate files** (plan Task 4) —
   `fs::read(...).ok()` in `transport/lapin.rs` silently connects without the
   configured CA; return a typed error identifying the exact path.
6. **Poison-message warning on permissive defaults** (plan Task 6) —
   `delivery_limit=null` + `dead_letter=null` means infinite redelivery for
   worker-crashing messages; emit a production warning (opt-out
   `production_warning`).

### F2 — hardening (after Round 2 + re-bench)

7. **Lazy consumer establishment** (plan Task 7) — `recover_generation`
   (`recovery_coordinator.rs:406`) consumes every declared worker profile, so a
   publisher-only process retains unacked messages on all queues at each
   reconnection; only establish requested profiles.
8. **Wire duplicate measurement** (plan Task 9) — `record_duplicate()` has no
   callers; the at-least-once contract requires duplicates to be measurable.
9. **Drain native events on publish/consume paths** (plan Task 10) —
   `ConnectionStateChanged`/`BackpressureDetected` only fire inside `stats()`
   (`pool.rs:264-265`); extract a shared `EventBridge` used by `Pool` and
   `Consumer` so the README/operations.md promises become true.
10. **Blind mode byte budget** (plan Task 11) — `publish_blind`
    (`publisher/actor.rs:229`) bypasses the byte budget (message-count only);
    reuse `with_byte_budget` (`publisher/mod.rs:245`). Includes a benchmark
    non-regression step.
11. **TLS integration: SNI + verify + lab TLS profile** (plan Task 12) —
    `TlsVerify::None` and `server_name` are validated but never read by the
    transport; API decision gated on lapin 4.10 connector capabilities, TLS
    profile in the lab, handshake/untrusted-CA/SNI tests.
12. **PIE end-to-end validation** (plan Task 13) — NTS naming inconsistency
    between `release.yml` and `package-pie-binary.sh`; no `pie install` test in
    CI. Unify naming, add a blocking CI job on a draft release. *Can start in
    parallel from F1 onward (external latency: release cycle).*
13. **Laravel contracts: `ClearableQueue` + optional `auto_subscribe`** (plan
    Task 14) — `queue:clear` fails (interface not declared) and `pop()` refuses
    plain queue names (deviation from the Laravel convention).

### F3 — ecosystem & DX (with F2)

15. **Log facade, typed coordinator errors, zero panics reachable from PHP**
    (plan Task 15) — `eprintln!`, `CoordinatorError = String`, `expect()`/panics
    in spawned tasks (process abort under FFI).
16. **Version alignment + CHANGELOG** (plan Task 16) — composer `^0.0` vs
    exception `^1.0` vs docs `1.0.0` vs workspace `0.0.7`.
17. **Install friction + static analysis** (plan Task 17) — `ext-rabbit_rs` as a
    hard `require` breaks `composer install` without the extension; move to
    `suggest` + runtime validation at connection resolution; add Pint + PHPStan
    to the quality gate.
18. **Harden the worker supervisor** (plan Task 18) — pcntl-free `--workers=1`
    path (the current error message is wrong) and non-blocking restart backoff.
19. **Docs/stubs alignment** (plan Task 19) — `stats()` stub documents 8 keys
    for 17 real ones; prefetch 16 advertised vs 64 configured; PHPUnit suite
    references that do not exist (Pest is used); nonexistent `dispatchBatch` API
    in docs.
20. **ZTS decision applied** (plan Task 20) — Option A: drop ZTS from the V1
    release matrix (16 → 8 assets), revisit in V2. See Parked.

Success criteria: all plan tasks green (tasks 1–7, 9–20), full quality gate, no
benchmark regression against the frozen budgets, `pie install` validated on a real
release.

## Round G — audit hardening (audit 2026-08-31, tasks 21–37)

Motivation: full technical audit of 2026-08-31 (see Landed). Findings not already
covered by Round F were split into 17 issues (#66–#82) organized as **7 file-disjoint
parallel tracks** — maximum concurrency, minimal interference; sequence only within a
track. Strategy decision (2026-08-31): **stabilize before features** — Round G contains
no features; post-1.0 feature ideas are parked below.

Status (2026-09-02): Tasks 21–25 (#66–#70), 26 (#71), 27 (#72), 28 (#73),
29 (#74), 30 (#75), 31 (#76), 32 (#77), 33 (#78), 34 (#79), 35 (#80),
38 (#83), 40 (#88), 41 (#89), 42 (#90) and Round C (#40) landed (see Landed),
plus promoted P1s #56/#52 and follow-ups #95/#96/#50. Red-team tasks 39–42
(#87–#90) are all delivered. Shipped in release v0.0.9. Round G exit landed
2026-09-02: #97 (PR #124), #81 (PR #125) and #82 (PR #131 — hygiene sweep,
one commit per item; cargo deny joined the gate, nextest retries=0,
coverage-laravel integration pass boots the lab itself). **Round G is
complete.**

### G0 — P0/P1 (delivery contract & availability)

21. **[P0][core] Transport liveness** (#66) — connection loss is never detected:
    the connection actor only listens to commands, `TransportConnection` has no
    error surface, lapin runs with `auto_recover = false`, consumer streams end
    silently. A routine broker restart stops consumption and bricks publishing
    in every PHP process until restart. Fix: transport liveness signal →
    `connection_lost`, stream termination as a trigger, recovery-failure counter.
22. **[P1][core] Publisher wake-up** (#67) — swallowed `Ready` event errors and
    `delay.mode=auto` (= plugin) suspending the publisher forever on a declare
    failure. Fix: propagate errors (generation rollback), probe the plugin in
    auto mode, never suspend for one message's topology failure.
23. **[P1][laravel][core] Consumer re-join** (#68) — one-shot `SourceReplaced`
    + PHP consumer cache never evicted: workers stop consuming after every
    recovery. Fix: evict on `SourceReplaced`/`Closed`. Depends on 21.
24. **[P1][ci] CI runs the real thing** (#69) — `--features integration` in CI
    (suites are currently compiled out while the lab boots), Laravel
    integration suite wired, `|| true` removed from coverage, PHPT scenarios
    committed and wired. Pairs with Round C (#40).
25. **[P1][core] Poison messages settle terminally** (#70) — attempts cap
    fabrication, capped delayed-release hot loop, unmarshable payload never
    settled. Fix: configurable `max_attempts`, terminal settlement
    (`reject(requeue=false)` → DLX). Complements #48.
26. **[P1][core] Consumer bounds** (#71) — unbounded `pending_incoming` under
    `no_ack` (documented guard never validated) + lossy `Drop` close.
27. **[P1][core] DLQ bindings** (#72) — bindings lost for the 2nd+ subscription
    sharing a DLQ with per-source routing keys → silent poison loss.
28. **[P1][core] Delay validation** (#73) — delay > max TTL bucket currently
    publishes immediately (route error swallowed); validate at the boundary.
29. **[P1][laravel] Supervisor clean exits** (#74) — `--max-jobs` recycling
    burns restart budget and stops the fleet after `--max-restarts`.

### G1 — P2 hardening (parallel with G0 except where noted)

30. **[P2][php-ext] Event callbacks** (#75) — callback exceptions destroyed,
    single-slot theft, drain starvation on the `next()` fast path.
31. **[P2][php-ext] Boundary correctness pack** (#76) — debug-only key
    validation, `ackBatch` side effects, dropped array/table headers, teardown
    flush budget + silent drops counter.
32. **[P2][core] Admin ops through recovery** (#77) — `size`/`purge` cache a
    raw connection forever; second AMQP connection per vhost.
33. **[P2][core] Executable config surface** (#78) — dead `mandatory` flag
    honored-or-rejected; `confirm_timeout: 0` / `heartbeat: 0` rejected.
34. **[P2][core] Delay queue identity + GC** (#79) — args not in the queue
    name (406 storms on rolling config changes); no GC of synthesized queues.
35. **[P2][laravel] Config lifecycle** (#80) — boot-time normalization blast
    radius; Octane reload keeps stale normalized config.
36. **[P2][laravel] Cross-process duplicates monitoring** (#81) — status
    command reports zeros; needs an exporter or management-API aggregation.
    After #50.
37. **[P3][ops] Hygiene pack** (#82) — dependabot lock, `cargo deny` in
    `check.sh`, nextest retries=0, llvm-from-rustup, coverage-laravel
    extension loading, lab-leak trap, bash-3.2 compat, `symfony/process`,
    dead code, test leftovers.

Success criteria: each issue is TDD per the ground rules; the Round G exit
criterion is the audit's P0/P1 list at zero, with the CI truth issue (#69)
proving fixes against a real broker.

## Round H — connection-first Laravel config (DX, spec 2026-08-31)

**DELIVERED 2026-09-02** (PR #130, v0.1.0 on main, tag not yet cut) — the
config redesign portion of this section is done: connection-first
`queue.connections.*`, `ConnectionCompiler` (lazy compile at `connect()`),
`config/rabbit-rs.php` rewritten to flat defaults, work-command fan-out
(spec §5 amended to the delivered one-child-per-connection mechanism),
subscriptions escape hatch, docs + CHANGELOG v0.1.0 migration table.
Companions #84 (topology preflight) and #85 (Kubernetes probes) remain
PARKED per the external review's feature freeze.

Motivation: two config homes (`queue.php` thin vs a 429-line `rabbit-rs.php`
with three hand-linked namespaces), boot-time normalization blast radius
(audit F-27/F-28). Full approved spec:
`docs/superpowers/specs/2026-08-31-laravel-config-redesign-design.md`.

Scope: one config home in `queue.connections.*` (SQS/redis idiom; multi-broker
= multiple connections); `config/rabbit-rs.php` becomes ~40 lines of
cross-cutting defaults; `ConnectionCompiler` replaces `ConfigNormalizer` and
normalizes lazily per connection at `connect()` — **absorbs #80** by
construction; env strings accepted and cast inside; dead knobs dropped from
the published surface (complements #78 on the core side); work profile =
connection name; multi-consumer via standard `queue:work` unchanged
(per-process pools, per-channel consumer tags — verified collision-safe).
Competitor takeaways kept: `connection_name` (management UI label),
env-string casting responsibility inside the driver. Rejected from competitor
surfaces: pool knobs, AMQP transactions, failed dual-sink, poll/consume modes.

Success criteria: compiler unit tests (defaults merge, env strings, escape
hatch, exact error paths); two-connection integration test on the lab;
`config/rabbit-rs.php` ≤ 50 lines; runnable config examples in
`docs/configuration.md` (single broker, multi-broker, env strings) plus an
updated README quickstart; CHANGELOG v0.1.0 breaking entry. Core and
extension untouched.

Companion (from the 2026-08-31 competitor survey, #84): **`rabbit-rs:topology`**
preflight command — verifies the compiled topology plan against the broker
(exchanges/queues/bindings/arguments), prints actionable gaps, exit 1 for CI/CD
deploy gates; `--fix` declares missing parts; doctor checks (extension loaded,
per-connection config compiles) included. Sequenced with the config redesign
since it reads the connection-first config.

Companion (design validated 2026-08-31, #85): **Kubernetes probes** — the
worker exports its state to a per-PID JSON statefile (~1s heartbeat, atomic
rename) in `storage/framework/rabbit-rs/probes/`; exec probes run in separate
processes, so the file is the only cheap channel (Horizon precedent).
`rabbit-rs:probe {startup|ready|alive|prestop}` reads it: `alive` = freshness
only (never the broker — restart-storm trap), `ready` = fresh + `connected`
(worker-side ~30s hysteresis), `startup` = boot complete, `prestop` = SIGTERM
to workers + bounded wait for drain. Hook point: `RabbitRsProbeEvaluated`
event with mutable `verdict` (maintenance mode, external flags). Sequenced
after Round G Track A (#66–#68): readiness needs the worker to know its real
connection state.

## Round C — local integration harness reliability

Motivation: 8 Laravel integration tests fail on this machine for pre-existing,
environment-level reasons (protected `$app` chaos, multi-node ConnectionException,
TypeError whoops). CI is green, so this is local-only — but it blinds local
regression detection for every future round that touches consumer/connection code.

Scope: diagnose each failure class; fix the harness/environment (not product code);
either reach a locally green `./scripts/test-integration.sh` on a clean environment or
document explicit, honest skip criteria.

Success criteria: integration suite reproducibly green locally, or documented skip
criteria enforced by the harness.

## Round D — dispatch gap investigation

**Promoted to P0 pre-1.0 by the external review of 2026-09-02** (was
post-1.0 performance work); sequenced after Round I, before Round J. Scope
extended with the review's batching/pipelining questions — see the tracker
comment on #41.

Motivation: the publish-side gap is now precisely localized. In fire-and-forget
rabbit-rs already leads (framework: 76 794 vs 31 182 ops/s vs vladimir, ~2.46×;
transport: ~2.8×) — nothing to optimize there. The whole remaining gap is the
**safe per-message confirm+mandatory path**: 0.31× vladimir at framework level
(9 772 vs 31 182 ops/s, same-session interleaved), 0.28× at transport level
(8 213 vs 29 648). goopil's own safety ladder prices it: blind 76.8k → unsafe
62.5k (×1.23) → safe 9.8k (×6.4) — the unsafe→safe step is the target.
Secondary target: batch-confirm at transport level, 0.68× vs bunny/amqplib.

Precondition: Round 2 landed and re-benched (clean consumer baseline); now
also **Round I (#126)** — profile and optimize against a consumer that no
longer needs workarounds.

Scope: profile the safe publish path end to end — flamegraphs plus per-stage
breakdown (PHP extension boundary, pump hand-off per message, confirm waiter,
mandatory-return handling, socket write) — then either implement targeted
optimizations (with re-bench proof) or document the ceiling with data.

Success criteria: root cause documented with measurements; either an implemented,
bench-validated optimization or a documented, quantified ceiling.

## Round E — adaptive prefetch per subscription

Motivation: fixed prefetch cannot serve opposite queue profiles at once — fast jobs
under-fill the pipeline (RTT visible, throughput capped at `prefetch / job duration`)
while slow jobs with high prefetch waste memory and amplify the post-crash redelivery
blast radius. Anticipated by the original design ("adaptive prefetch based on EWMA,
target buffer time, hysteresis").

Full design (approved spec):
`docs/superpowers/specs/2026-08-29-adaptive-prefetch-design.md`; implementation
plan: `docs/superpowers/plans/2026-08-29-adaptive-prefetch.md`.

Scope: per-subscription adaptive prefetch driven by an EWMA of settlement latency
(includes PHP job duration), target buffer time, 25% hysteresis, 1s tick in the
consumer actor with `set_qos` applied off the critical path; union wire format
(`fixed`/`adaptive`) backward compatible with the plain int form; adaptive rejected
with `early_ack`/`no_ack` at validation; buffer sizing from the sum of maxima;
`Consumer::getPrefetchStats()` observability; Laravel normalizer + docs.

Success criteria: full quality gate green; deterministic paused-time tests prove the
QoS adjustment sequence on the mock transport; fixed mode behavior byte-identical
(regression tests).

## Order of execution (agreed 2026-08-30, updated 2026-09-02)

~~Round 2 (starting with the error_tx drop-oldest fix as the P1 hypothesis) →
Round F1 → Round C → Round F2 → Round E → Round D~~ — Round 2 and Round F are
landed (v0.0.8).

Current queue (updated 2026-09-02 after the external review — **stop
features → fix consumer → optimize confirms → prove results → ship 1.0**):

1. **Round I (#126, P0)** — consumer correctness under stress: #111 first
   (it also flakes the ext Pest suite ~1 run in 4), then soak/chaos
   verification of the Round 2 stall fix, reconnect + generation-aware ACK
   proof, benchmark workaround removal.
2. **Round D (#41, P0, promoted)** — confirmed-publish
   batching/pipelining; profile before optimizing.
3. **Round J (#127–#129, P1)** — benchmark proof, installation matrix,
   README repositioning (file-disjoint tracks; #128 has external CI
   latency and can start in parallel).
4. **Round G exit**: only #82 remains (P3/P4 ops sweep, last — it also
    sweeps wave debris). F2/F3 leftovers (#53, #58, #60) fill capacity.
5. ~~Round H (v0.1.0)~~ — **landed 2026-09-02** (PR #130, on main; tag
   v0.1.0 to cut with the next release).

Feature freeze until the P0s land: Round E (#42) and the other parked ideas
(Kubernetes probes #85, topology command #84, Prometheus, realtime stack)
stay parked — the review's P2 list confirms the freeze.

Conflict points between tracks (rebase or sequence): #66 ↔ #71 share
`consumer/set.rs`; #67 ↔ #73 ↔ #76 share `publisher/actor.rs` /
`conversion.rs`.

1.0 gate (agreed 2026-08-31, extended 2026-09-02 with the review's DoD):
**1.0 = Round G exit + Round H landed + Round I/J delivered.** The
Round G exit criterion is the audit's P0/P1 list at zero, with #69 proving
fixes against a real broker; Round H (v0.1.0, connection-first config) ships
the last breaking change before the tag. The review's 1.0 Definition of
Done, on top of that gate:

- **Correctness**: no known missed-delivery race; no known ACK stall;
  deterministic reconnect; generation-safe ACKs; recovery tested under
  load; long-duration tests.
- **Reliability**: confirm failures tested; replay tested; duplicate
  semantics documented; crash limitations documented; graceful shutdown
  validated.
- **Performance**: benchmarks reproducible; consumer advantage
  demonstrated; publish modes clearly compared; confirm batching studied
  (Round D); p50/p95/p99 available.
- **Packaging**: architectures + PHP versions validated (Round J2);
  installation documented; upgrade + rollback tested.
- **Operations**: long-running workers; RabbitMQ reconnect; graceful
  shutdown; status/health checks; usable metrics.

No breaking changes after 1.0 — features (realtime stack, multi-framework
adapters) come after.

## Post-1.0 feature ideas (parked — recorded so nothing is lost)

From the native design ("Planned evolutions") and review discussions. None of
this starts before the Round G stabilization exit criterion.

- **Prometheus / OpenTelemetry exporters** — required by #81 (cross-process
  duplicates monitoring); design the metrics surface once, export twice.
- **Multiprocess `rabbit-rs:work`** (design milestone 2) — supervisor already
  exists (WorkerSupervisor); the remaining scope is advanced subscription
  selection and multiprocess mode on top of it.
- **Adaptive prefetch** — Round E (#42), design and plan already approved.
- **Additional routing and failover strategies** (host selection beyond the
  current list rotation).
- **Alternative AMQP backend** — only if Round D profiles justify it; the
  `Transport` abstraction keeps this cheap.
- **RabbitMQ Streams support** — distinct product if a real need appears
  (design decision, out of the core crate).
- **Per-queue publish safety inside one connection** — see Parked below;
  arbitrate alongside Round D.
- **Symfony Messenger transport (or other-framework adapters)** — thin
  adapter over the extension mirroring the connection-first config surface;
  only after Laravel consolidation (Round H) and 1.0, and after the
  extension boundary hardening (#75, #76).
- **Worker batched prefetch in `pop()`** (2026-09-01, smoke-benchmarked) —
  `pop()` pays one PHP↔Rust crossing per job (`Consumer::next()`, ~60µs
  measured), capping the unit-consume shape at ~16–22k jobs/s while the same
  smoke's `nextBatch(256)` + single `ackThrough` path sustains 41k+/s
  (fire-and-forget consume 16.7k/s vs batch-confirm 41.2k/s; the Round 2
  baseline shows the same inversion, so it is structural, not a regression).
  Plan: `pop()` fills a bounded per-profile FIFO via `nextBatch(≤prefetch,
  blockForMs)` and hands jobs out one at a time; acks stay per-job at settle
  (no `ackThrough` — out-of-order job completion would ack unfinished jobs);
  the buffer is bounded by the configured prefetch, a crash redelivers it
  (at-least-once intact, duplicates measurable), and the #68 consumer-cache
  eviction rules extend to the buffer. Escalation if ever insufficient: a
  batch-drain child mode inside the **existing** `rabbit-rs:work` supervisor
  (the child loop switches from the standard `queue:work` pop-per-job to
  `nextBatch` + batch-completion acks) — parked as an idea only: it departs
  from standard `queue:work` semantics and weakens ack-after-job-completion.
  Sequenced post-1.0 (parked with #41/#42).
- **Profile `Consumer::next()` per-call cost** (2026-09-01) — evaluation, to
  run once everything else is done (Round G exit + Round H landed): ~60µs per
  unit `next()` call is high for an ext-php-rs boundary; attribute the cost
  (tokio channel hop, Zval marshalling, delivery object construction) with
  `cargo bench`/profiling before deciding on a cheaper core fast path that
  would benefit every consumer shape.

### Realtime stack (2026-08-31) — suggested order 1 → 2 → 3 (each builds on the previous)

1. **Request/reply (RPC) integrated up to Laravel** — typed
   `RabbitMqRpc::call(queue, payload, timeout)` on the AMQP direct reply-to
   pattern (`amq.rabbitmq.reply-to` pseudo-queue): core gets `reply_to` support
   (`correlation_id` already exists in `MessageProperties`), the extension gets
   a blocking-with-deadline call API, Laravel gets a handler API that answers
   on the reply queue. Semantics differ from the queue contract — calls are
   at-most-once with a typed timeout error; document the broker-restart
   behavior explicitly. Foundation for the two items below (replies ride it).
2. **Pusher-style front client over MQTT** — browser client (Pusher-like
   ergonomics) speaking MQTT over WebSocket through RabbitMQ's `rabbitmq_mqtt`
   plugin: front → MQTT → RabbitMQ → Laravel consumers, and back via publish.
   Scope notes: auth story (per-user credentials from a signed endpoint),
   MQTT QoS 0/1 vs the at-least-once contract, topic↔exchange mapping, small
   JS client library, `rabbitmq_mqtt` lab profile.
3. **Laravel Echo compatibility layer** — a server speaking enough of the
   Pusher protocol for `laravel-echo` (private/presence channels, signed auth
   endpoint) on top of the MQTT bridge, with channels mapped to
   queues/topics; reuse Laravel's broadcast auth conventions. Depends on 2.

- *(slot reserved — add new ideas here with a date and one-line motivation.)*

## Parked (no round yet)

- **Per-queue publish safety**: `publisher.safety` is a connection-level setting
  (ConfigNormalizer validates safe|unsafe|blind; the core applies one SafetyMode
  per connection/vhost). Several Laravel connections (same broker, distinct
  vhosts) already give per-vhost safety today. A true per-queue safety inside one
  connection would be a core config-surface extension — arbitrate alongside
  Round D.
- Publish latency excludes the terminal flush (comment if ever reported).
- Re-fetch acquisition blocks on all brokers when a source retires — semantics match
  mono-broker behavior and are documented; revisit only if an async retire is needed.
- **ZTS decision (APPLIED for V1, plan Task 20 — Option A)**: ZTS was
  unproven — the process-global `RuntimeRegistry` is shared across PHP threads,
  the CI ZTS job was advisory-only (`continue-on-error`) and release ZTS
  artifacts only got an `extension_loaded` smoke test. Applied for V1: ZTS
  dropped from the release matrix (16 → 8 assets, `support-zts: false` in the
  root `composer.json`) and the advisory ZTS CI job removed. Revisit in V2 with
  thread isolation (TSRM-aware registry) + a blocking ZTS CI job + real
  concurrency tests.
- **Known minor items from the audit, deliberately unscheduled**:
  `try_publish_hot` is a pure passthrough of `try_publish` (the announced hot path
  does not exist yet); `frame_max = 1 MiB` is hardcoded in `transport/lapin.rs`;
  AGENTS.md says 6 core test files but 8 exist (blind_pump, transport_tuning).
- **Release ordering vs design doc (documented trade-off, audit F-26)**: the
  design doc wants the release published only after all gates; `verify-pie-install`
  runs after `publish-release` because PIE 1.4.10 cannot resolve draft releases —
  the verification still gates Homebrew + the Laravel split. Revisit the DAG when
  PIE supports draft resolution.
- **Micro-optimizations (audit F-38), unscheduled**: dead `exchange`/`routing_key`
  copies per delivery, scheduler O(n²) `contains` + per-dispatch alloc (largely
  superseded by Round E's scheduler rework), O(n) replay-deadline scan while
  suspended, double property clones, `flush_batch` deep clones, `early_ack`
  spawn-per-message, per-publish `EventBridge::drain` without callbacks. Each
  sub-µs to µs — do only with a profile, after #41.

## Housekeeping (owner actions)

- ~~`git pull` the main checkout~~ (done 2026-08-30 — main checkout is current).
- Delete the orphan SonarCloud project `Goopil_rabbit-rs` and the now-unused
  `SONAR_TOKEN` secret (coverage goes to Codecov only; SonarCloud runs automatic
  analysis on `Goopil_php-rabbit-rs`).
