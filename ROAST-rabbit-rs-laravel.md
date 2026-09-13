# Code Review — rabbit-rs-laravel (goopil/rabbit-rs-laravel)

> **Scope**: `packages/laravel-queue` (Laravel 12/13 queue driver on the native `ext-rabbit_rs` extension, at-least-once contract).
> **Branch reviewed**: `ref/sonarcloud-scope-and-fixes` — 27 src files, 5,179 LOC, Pest suite.
> **Status re-checked against v0.3.2** (2026-09-12): findings re-verified at the tag; each HIGH/MEDIUM carries its current status.
> **Method**: full adversarial review (src + docs + tests/CI) + independent verification of HIGH findings.
> Findings marked ✅ verified line-by-line against the code.

**Verdict: 2 HIGH, 6 MEDIUM, 4 LOW.** The config compiler and the queue/attempts semantics are remarkably clean and well tested — it's `bulk()` and signal handling that break the documented promises.
**v0.3.2 status**: v0.3.2 ships the auto-scaling supervisor (#262 — `--min-workers/--max-workers/--once/--stop-when-empty`, non-blocking downscale SIGTERM) plus tests/benches/deps; **no fix for any finding below** — `RabbitMqQueue.php` is untouched (HIGH 1, HIGH 2 still open), the new supervisor's own shutdown path keeps the sequential blocking stop (MEDIUM 6, re-anchored below). MEDIUM 3 superseded by the BREAKING `auto_subscribe` removal (#228); MEDIUM 4 and 7 partially improved; HIGH 1, HIGH 2, MEDIUM 5, 6, 8 still open (re-verified at the tag). Note: `RabbitMqServiceProvider::EXTENSION_CONSTRAINT` is now `^0.3.2` — the laravel and native packages must move together.
**Post-v0.3.3 status**: none of the findings below are fixed yet; additionally, `rabbit-rs:topology --fix` now re-verifies every declared object (passive queue probe + management API) before printing `topology declared` and exits non-zero per object otherwise (#273) — hardening the repair path the "Verified clean" section covered, not one of these findings.

---

## 🏗️ Architecture

- **The native error drain is a structural leak point**: it has a single entry point (`drainSettlementErrors()`, called only by `pop()`). Any path that publishes without consuming (web/FPM, bulk, Octane) never drains — HIGH 1.
- **Three components disagree on the `queue` key** (`RabbitRsConnections`, `ConnectionCompiler`, `RabbitMqQueue`): the key feeds the worker plan but neither the topology nor `pop()` — MEDIUM 4.
- **The docs are excellent** (reference.md, 1,847 lines) but 3 doc/code gaps found (MEDIUM 3, 5, 12) — the "doc = contract" pattern holds 97% of the time, which is better than most packages, hence the importance of fixing the remaining 3%.

---

## 🔴 HIGH

### 1. ✅ STILL OPEN @v0.3.2 — `bulk()` in `safe` mode silently loses jobs in web/FPM

> **Status update (2026-09-13, branch `fix/safe-unroutable-surfacing`)** — the probe-F half (safe-mode unroutable publish lost when the process ends without a further queue operation) is fixed driver-side: `RabbitMqQueue::__destruct()` drains pending publish errors and logs each at error level (`docs/reference.md` "Where unroutable-publish failures surface"). Verified against HEAD code: `bulk()` itself is already sync-loud (`Pool::publishBatch` block_on's `publish_batch`, which awaits every outcome and throws on `Returned`), so the Octane cross-request detonation applies to the pipelined single-push path, not to bulk.
`src/RabbitMqQueue.php:349-405, 293-305` (@v0.3.1: `drainSettlementErrors()` still called only from `pop()` at `:448`)

`drainPublishErrors()` is `private` and only called by `drainSettlementErrors()`, itself called **only** by `pop()`:

```php
public function pop($queue = null, $index = 0)
{
    $this->drainSettlementErrors();   // the only drain entry point
```

Yet `publishBatch()` is async on the native side (the stub documents it: unconfirmed outcomes "would otherwise surface as exceptions at the next publish/flush/size/clear/stats operation"). An HTTP request that `bulk()`s N jobs and never pops: the process dies before any subsequent operation → records destroyed (`dropped_publications_total`). Under Octane, the stale error **detonates in an unrelated request** instead. Net result: HTTP 200 with message_ids returned, jobs never reached the broker, no exception, no log — in the `safe` mode that exists precisely to prevent this. Single `push()` is healthy (synchronous confirmation); `bulk()` is not. Fix: drain in a `__destruct`/`terminating` hook, or make the batch confirmation synchronous at the end of `bulk()`.

### 2. ✅ STILL OPEN @v0.3.2 — `bulk()` is not chunked against the native 256-message / 1 MiB batch cap — and the cap is undocumented
`src/RabbitMqQueue.php:305` (@v0.3.1: `:338`, zero `array_chunk` hits) + stub `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php:369`

```php
$messageIds = $this->pool->publishBatch(array_column($messages, 'native'));
```

No `array_chunk`, no size accounting anywhere in `src/` (zero hits for "chunk"). The 256/1 MiB cap appears **nowhere** in the README or reference.md. `Queue::connection(...)->bulk([...257 jobs])` — including every `Bus::batch()` dispatch, which funnels through `Queue::bulk()` — throws an opaque native error. One-line fix: `array_chunk($messages, 256)` + a doc line + a test above 256.

---

## 🟡 MEDIUM

3. **FIXED IN 0.3.0 (BREAKING, #228) — superseded**: `auto_subscribe` is removed entirely in the 0.3.x contract. A connection carrying the key (any value, including stale package defaults) now **fails compilation** with an actionable error; `RABBIT_RS_AUTO_SUBSCRIBE` is gone from the package config; multi-queue pop scoping is unaffected. Verified live during the 0.3.1 upgrade of a consumer project: the compile-time rejection message names the option and the migration path (`queue` key or `subscriptions`). The compiler fallback and the test pinning it (both quoted below, pre-0.3.0) are obsolete:

   ~~`src/Config/ConnectionCompiler.php:74` — fallback `auto_subscribe ?? true` contradicts the docs (`false`, reference.md:461/:666) and the "explicit null wins" rule (reference.md:517); `ConnectionCompilerTest.php:91-94` pins the opposite of the doc.~~
4. **PARTIALLY IMPROVED in 0.2.x/0.3.x — `RabbitRsConnections.php:100-104` vs `ConnectionCompiler.php:277-285` vs `RabbitMqQueue.php:412-413`** — A `queue` key disjoint from `subscriptions` is still never validated by the compiler, and three components still disagree: (a) the worker plan schedules the `queue` key, (b) the compiler **replaces** the derived subscription when `subscriptions` exists → the `queue` key's queue is never declared/bound, (c) `pop()` of that queue silently consumes **every** subscription of the profile (the `hasProfile()` fallback at `:465` @v0.3.1 matches the profile named after the connection). What improved: 0.2.1's #207/#183 scoped pops on multi-queue profiles, and 0.3.x now **throws** `InvalidArgumentException` for queue names no profile covers (`:468`) instead of failing mysteriously; 0.2.1's #220 also made `rabbit-rs:topology verify` check the publish-route exchange and per-subscription bindings, so the unroutable-publish half is caught in verify mode. Remaining: the silent whole-profile consumption when the queue key name equals the profile name, and no compiler/doctor check flags the disjoint key.
5. **✅ STILL OPEN @v0.3.2 — `src/Connectors/RabbitMqConnector.php:52-58`** — `after_commit`/`block_for` reject `env()` strings (strict `is_bool`/`is_int`) while reference.md:481-492 promises lazy casting ("`"1"`, `"true"`… / `"64"`, `"-1"`") that every other key follows via `Values::boolean`/`Values::integer`. The documented wiring `'after_commit' => env('RABBIT_RS_AFTER_COMMIT')` **throws InvalidArgumentException** at first resolution in production. Align on `Values::*`.
6. **✅ STILL OPEN @v0.3.2 — `src/Console/WorkerSupervisor.php:762-770` (@v0.3.1: `stopAllProcesses` at `:410-417`, still a sequential `foreach { $process->stop(10, SIGTERM) }`)** — Blocking native calls (`next(block_for)`, `consumer()` with `wait_timeout` default 30 s) freeze the Zend VM → `pcntl_async_signals` handlers never run → a child parked in `next(30)` never observes the supervisor's SIGTERM within its 10 s window → **SIGKILL** (in-flight job unacked: at-least-once holds, but the "wait for current jobs" promise in reference.md:1159-1160 is broken). Also `stop(10)` is **sequential** per child: N wedged children = N×10 s, against your own docs' `TimeoutStopSec=30` recommendation. Fix: parallel stop + overall budget, and/or make `next()` interruptible on the native side. **v0.3.2 delta (#262):** the supervisor was rewritten for auto-scaling — the *downscale* path is now a non-blocking `posix_kill(SIGTERM)` with a 15 s SIGKILL escalation (`releaseIdleSlots`), but the supervisor's *own shutdown path* (`stopAllSlots`) keeps the same sequential blocking stop, so this finding stands.
7. **PARTIALLY IMPROVED in 0.3.x — `tests/Integration/QueueWorkerTest.php:9` + `phpunit.xml:7-12`** — The 13,813 LOC of tests are honest (Toxiproxy chaos with a fingerprint that **hard-fails** on a foreign proxy, supervisor tested with real subprocesses). What improved: the 0.3.x CI restructure now runs the full integration suite explicitly (`functional-matrix.yml` `integration-php85` job running `scripts/test-integration.sh` against a fresh lab, plus the CI `integration` job on PHP 8.4) — a stale extension artifact no longer silently greens the proof in CI. What remains: `phpunit.xml` still only declares Unit + Feature, so a bare local `vendor/bin/pest` never touches `tests/Integration` (every file gates on `skip('ext-rabbit_rs is required')`), and nothing asserts a minimum executed-test count on the Integration suite itself.
8. **✅ STILL OPEN @v0.3.2 — `src/Console/RabbitMqDoctorCommand.php:247` (@v0.3.1: `:171, 246`)** + `DoctorProbe.php:45-50` — The doctor only probes **one** queue (`queueName()` resolves `workers[0].subscriptions[0]`) yet emits `ok topology verify: …` for all of them. In `verify` mode — the drift-detection mode — a missing 2nd/3rd queue passes green. `DoctorProbe::queueSize()` exists and `rabbit-rs:topology` uses it per-queue; the doctor doesn't.

---

## 🔵 LOW

9. **STILL OPEN @v0.3.2 (`RabbitMqWorkCommand.php:20`, @v0.3.1: `:18`) — `{--timeout=60 : The number of seconds a child process can run}`**: wrong. Laravel's `--timeout` is the **per-job** timeout (pcntl_alarm around `fire()`); "child process lifetime" is `--max-time`. An operator bounding children with this leaves hung jobs without an alarm.
10. **✅ STILL OPEN @v0.3.2, verified live — `src/Console/RabbitMqDoctorCommand.php:422-430` (@v0.3.1: `:374-385`)** — `balance=auto` flagged as a warning while `readyNow()` exists (Horizon/RabbitMqQueue.php:119-122) and the message itself says "available since 0.1.2". Observed on a real 0.3.1 deployment during upgrade validation: every legitimate Horizon `balance=auto` setup permanently pollutes CI output with this warning. Stale message.
11. **`src/Console/WorkerSupervisor.php:301-305`** — Hot-restart loop on clean exit: a child that exits 0 fast and forever (e.g. `--memory=0` → instant `memoryExceeded()`) → respawn with no backoff, the `restartCounts` budget reset every time. Combined with MEDIUM 6, can exceed the process manager's grace window.
12. **`README.md:120`** — Two env table rows merged (double pipe): the `RABBIT_RS_PRODUCTION_WARNING` row is lost when rendered. And `RABBIT_RS_AUTO_SUBSCRIBE` (config/rabbit-rs.php:51, reference.md:705) has no row at all — the README env surface is incomplete exactly where the driver's most surprising default lives.

---

## ✅ Verified clean

- **attempts()/maxTries**: broker attempts are 1-based, exact parity with `Worker.php:713` and backoff indexing (`$backoff[$job->attempts() - 1]`). No off-by-one.
- **release(delay)**: delegates to native with delay-strategy validation — no in-memory timer to lose on restart.
- **Double-processing**: no driver-side expiry (the broker owns redelivery), quorum `delivery_limit` + DLX enforced at compile (`ConnectionCompiler.php:538-544`), unmarshable deliveries settled terminally and loudly.
- **ConnectionCompiler**: hosts/IPv6/ports, per-sub-key merges, adaptive-prefetch rules, `early_ack`/`no_ack` gating, exact-path errors — solid and well tested (800+ lines).
- **Horizon**: `JobPending/Pushed/Reserved/Deleted` event ordering = Redis parity; the `MarshalFailedEvent` gap (RedisJob-only) correctly bridged by the manual `JobFailed` dispatch; `readyNow()` present.
- **Octane lifecycle**: `connected()` guard against resolution during flush, fork-safe pool factory via PID check.
- **Provider/binding**: unique connector name, lazy compilation, extension check at boot AND invocation.
- **Destructive ops**: no delete anywhere in commands; `--fix` mode-gated with `--force` as documented.

---

## Recommended priorities

1. **HIGH 1**: make the drain unavoidable (Octane terminating callback + drain at the end of `bulk()`/`push()` in safe mode, or synchronous batch confirmation).
2. **HIGH 2**: `array_chunk` + document the 256/1 MiB cap + a test above 256.
3. **MEDIUM 6**: parallel child stop + document the signal/blocking-native limit (or make `next()` interruptible in the extension).
4. **MEDIUM 5**: two-line fix — route `after_commit`/`block_for` through `Values::*` like every other key.
5. **MEDIUM 8**: probe every subscription queue in `rabbit-rs:doctor` (`DoctorProbe::queueSize()` already exists).
6. LOWs as you go (9 and 12 cost 2 minutes each); also update the stale `balance=auto` doctor message (10).
