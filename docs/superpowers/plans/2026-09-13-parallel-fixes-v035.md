# Parallel Fix Wave — v0.3.5 Implementation Plan (#287, #288, #285, #290)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended — one subagent per PR branch, dispatched in parallel per `dispatching-parallel-agents`) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four actionable open issues after v0.3.4 in one parallel wave — #287 (once-mode stale-exit regression), #288 (doctor canary self-sandbagging), #285 (typed admin errors), #290 (stats surface in PHP) — each as its own PR from its own worktree.

**Architecture:** Four independent PR branches cut from `origin/main`, developed in parallel in four git worktrees, merged in a fixed order. PHP fixes live in `packages/laravel-queue/src/**`; the Rust fix lives in `crates/rabbit-rs-core/src/client.rs`. A final integration task re-runs the playground roast validations against the merged main.

**Tech Stack:** PHP 8.5 / Laravel package (Pest tests), Rust workspace (`rabbit-rs-core`, `rabbit-rs-php`), RabbitMQ management API, GitHub CLI for PRs.

## Global Constraints

- Everything (code, commits, PRs, CHANGELOG) in **English**.
- **TDD** per root `AGENTS.md`: add the focused failing test first, observe the intended failure, implement minimally.
- PHP tests use **Pest** (`describe`/`it`/`expect`), never PHPUnit classes. Run from `packages/laravel-queue` (`vendor/bin/pest tests/...`). The native extension is faked by `tests/bootstrap.php` in Unit/Feature suites; Integration suites need the live broker (CI only).
- Rust: run `rtk cargo fmt --all` after any Rust edit; tests via `rtk cargo nextest run --workspace --all-targets --no-fail-fast`; quality gate `scripts/check.sh` before pushing.
- Commits: conventional and scoped (`fix(laravel): …`, `fix(core): …`, `test(laravel): …`). PRs are **squash-merged**, branch naming `fix/<topic>`, PR body ends with `Closes #NNN`.
- CHANGELOG: both `/Users/zacharyvolpi/dev/perso/rabbit-rs/CHANGELOG.md` and `packages/laravel-queue/CHANGELOG.md` currently start at `## [0.3.4] - 2026-09-13` with **no Unreleased section** — the first merged PR introduces `## [Unreleased]` (with `### Fixed` / `### Added`) at the top of both; later PRs append to it. Keep the one-bullet-per-issue narrative style with `(issue #NNN)` refs (see the #272 entry at root `CHANGELOG.md:15` for tone).
- `.ai/rules/` does not exist in this repo; root `AGENTS.md` is the rulebook.
- Worktree creation (from the main checkout): `git worktree add ../rabbit-rs-<name> -b <branch> origin/main`. Delete the worktree and branch after the PR merges.

## Wave structure

| PR | Branch | Worktree | Issue | Language | Files touched (no overlap except noted) |
|---|---|---|---|---|---|
| A | `fix/once-fresh-exit` | `../rabbit-rs-once-fresh-exit` | #287 | PHP | `QueueDepthSampler.php`, `RabbitMqWorkCommand.php`, `WorkerSupervisor.php`, + tests |
| B | `fix/doctor-canary-dlq` | `../rabbit-rs-canary-dlq` | #288 | PHP | `DoctorProbe.php` (canary methods only), `RabbitMqDoctorCommand.php` (ok text), + tests |
| C | `fix/admin-error-classification` | `../rabbit-rs-admin-errors` | #285 | Rust (+ zero PHP changes required) | `crates/rabbit-rs-core/src/client.rs`, core tests |
| D | `feat/pool-stats-surface` | `../rabbit-rs-stats-surface` | #290 | PHP | `RabbitMqQueue.php`, `RabbitMqStatusCommand.php`, + tests |

**Merge order: A → D → B → C.** Conflict map: B and C both touch `DoctorProbe.php` (B rewrites the canary methods, C touches `probePool()` at :325-339 only) — merging B before C keeps C's rebase trivial. A and D touch disjoint files. Each branch rebases onto `origin/main` before opening its PR if another has merged.

**Not in this wave:** #282 (perf investigation — separate plan for 0.4.0), #253's latency re-measure (folded into #282), 15.2 (documented limitation, non-goal).

---

## Task 1 (PR A, #287): fresh depth re-probe before the once-mode exit decision

**Files:**
- Modify: `packages/laravel-queue/src/Support/QueueDepthSampler.php` (public API + cache read gate, lines ~87-155)
- Modify: `packages/laravel-queue/src/Console/RabbitMqWorkCommand.php:129-134` (`depthCallback`)
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php:410-425` (`pendingDepth`), `:481-496` (exit decision in `runOneShot`)
- Test: `packages/laravel-queue/tests/Feature/QueueDepthSamplerTest.php` (add cases, style template: the failure-caching pin at :162-170)
- Test: `packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php` (add cases, templates: #269 test at :496-529, #272 sampler-through-supervisor test at :581-627, `makeSupervisor()` helper at :752-796)

**Interfaces:**
- Produces: `QueueDepthSampler::depths(bool $fresh = false): array` — when `$fresh` is true the native cache is bypassed for reads (results are still written to it).
- Produces: depth callback contract becomes `Closure(bool $fresh = false): array<string, int|null>`. Old zero-arg closures keep working (PHP ignores extra args to userland callables).
- Produces: `WorkerSupervisor::pendingDepth(bool $fresh = false): int` — returns **-1 when every lookup failed** (inconclusive), otherwise the summed positive depth. The caller at the exit decision is the only consumer of -1.

**Context for the implementer:** `QueueDepthSampler` memoizes the native probe per connection+queue for 2 s (`NATIVE_CACHE_TTL_SECONDS = 2.0`, line 54), **including failures and stale 0s** (lines 140-154). `WorkerSupervisor::runOneShot()` breaks at line 495 when `pendingDepth() == 0`; a cached 0 or cached null makes the final check report "drained" while the broker holds real work (issue #287: 215 jobs drained 204/4/6/1 over 4 passes). The management-API path (`Support/ManagementApi.php`) is never cached and requires `management_url`; do not touch it.

- [ ] **Step 0: Verify `pendingDepth()` has a single caller**

Run: `grep -rn "pendingDepth" packages/laravel-queue/src/`
If the scale pass (or anything else) also calls it, guard -1 at those call sites with `max(0, …)` so only the exit decision sees inconclusive. Otherwise proceed.

- [ ] **Step 1: Write the failing sampler test** — add to `tests/Feature/QueueDepthSamplerTest.php`:

```php
it('bypasses the native cache when asked for a fresh read', function () {
    $calls = 0;
    $answers = [0, 0, 3];   // probe 1 and 2 report an empty queue, probe 3 the real depth
    $native = static function (string $connection, string $queue) use (&$calls, &$answers): ?int {
        $calls++;

        return $answers[$calls - 1] ?? null;
    };
    $sampler = sampler([['connection' => 'mq', 'queues' => ['default']]], $native, 60.0);

    expect($sampler->depths())->toBe(['mq' => 0])            // probe 1, cached
        ->and($sampler->depths())->toBe(['mq' => 0])         // still cached, no second probe
        ->and($sampler->depths(fresh: true))->toBe(['mq' => 3']) // bypasses the cache, probe 3
        ->and($calls)->toBe(2)
        ->and($sampler->depths())->toBe(['mq' => 3]);        // fresh read re-primed the cache
});
```

- [ ] **Step 2: Run it** — `cd packages/laravel-queue && vendor/bin/pest tests/Feature/QueueDepthSamplerTest.php`
Expected: FAIL — `depths()` does not accept a `fresh` argument.

- [ ] **Step 3: Implement `fresh` on the sampler** — change the signature to `public function depths(bool $fresh = false): array` and gate the cache *read* (lines ~140-143) on `! $fresh`; the cache *write* after a successful probe stays as is. Update the class docblock: "pass fresh: true to bypass the memoized read for a single call — the supervisor's final once-mode drain check uses this so a stale 0 or a memoized failed probe cannot end the drain with work pending (issue #287)."

- [ ] **Step 4: Re-run the sampler suite** — expected PASS (the failure-caching pin at :162-170 must still pass).

- [ ] **Step 5: Write the failing supervisor tests** — add to `tests/Feature/WorkerSupervisorIntegrationTest.php`:

Test 1 — stale cached 0 must not strand work:

```php
it('once mode re-probes the depth fresh before its final drain check so a stale cached zero does not strand work', function () {
    $probes = 0;
    // 10 jobs. The memoized probe keeps answering 5→5 (its 2 s window), the
    // fresh exit re-check sees the real remaining depth and re-arms.
    $sampler = new QueueDepthSampler(
        [['connection' => 'rabbit-rs', 'queues' => ['default']]],
        static function (string $connection, string $queue) use (&$probes): ?int {
            $probes++;

            return $probes % 2 === 0 ? 0 : 3;   // cached reads see 3 once, then the stale 0
        },
        60.0,
    );
    $spawned = 0;
    $supervisor = makeSupervisor(
        once: true,
        depth: static function (bool $fresh = false) use ($sampler): array {
            return $sampler->depths($fresh);
        },
        processFactory: static function (int $workerIndex) use (&$spawned): Process {
            $spawned++;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        },
        maxWorkers: 3,
    );

    $exit = $supervisor->run();

    expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
        ->and($spawned)->toBeGreaterThan(1);   // the stale 0 alone would have exited after the first spawn
});
```

(Use `makeSupervisor()`'s real parameter names — read the helper at :752-796 first; the intent is: sampler TTL 60 so cached reads freeze, children exit 0, depth only revealed on fresh calls.)

Test 2 — inconclusive fresh probe stays bounded:

```php
it('once mode retries a fully failed fresh probe within the re-arm budget instead of reporting a drained plan', function () {
    $probes = 0;
    $sampler = new QueueDepthSampler(
        [['connection' => 'rabbit-rs', 'queues' => ['default']]],
        static function () use (&$probes): ?int {
            $probes++;

            return null;   // every probe fails
        },
        0.0,
    );
    $spawned = 0;
    $supervisor = makeSupervisor(
        once: true,
        depth: static function (bool $fresh = false) use ($sampler): array {
            return $sampler->depths($fresh);
        },
        processFactory: static function (int $workerIndex) use (&$spawned): Process {
            $spawned++;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        },
        maxWorkers: 3,
    );

    $exit = $supervisor->run();

    expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)   // bounded: retries, then a clean stop
        ->and($probes)->toBeLessThanOrEqual(20);         // no unbounded probe loop
});
```

(Adapt `makeSupervisor()`'s real parameter names from the helper at :752-796 — the intent is: `once: true`, the sampler behind the `depth` callback, children that exit 0 immediately, and a probe that always fails.)

- [ ] **Step 6: Run them** — expected FAIL (stale 0 exits after one spawn; second test may pass already — if so keep it as a pin).

- [ ] **Step 7: Implement** —
`RabbitMqWorkCommand::depthCallback()` (:129-134):

```php
private function depthCallback(array $plan): \Closure
{
    $sampler = new QueueDepthSampler($plan);

    return static fn (bool $fresh = false): array => $sampler->depths($fresh);
}
```

`WorkerSupervisor::pendingDepth()` (:410-425):

```php
/**
 * Total pending depth across the plan connections: the summed non-null
 * gauge values, or -1 when every lookup failed (inconclusive). Pass
 * fresh: true to bypass the depth sampler's memoized window — the final
 * once-mode drain check must not trust a cached 0 or a memoized failed
 * probe (issue #287).
 */
private function pendingDepth(bool $fresh = false): int
{
    $depthCallback = $this->depthCallback;
    if ($depthCallback === null) {
        return 0;
    }

    $pending = 0;
    $known = false;
    foreach ($depthCallback($fresh) as $depth) {
        if (is_int($depth)) {
            $known = true;
            if ($depth > 0) {
                $pending += $depth;
            }
        }
    }

    return $known ? $pending : -1;
}
```

`runOneShot()` exit decision (:481-496):

```php
if ($slots === []) {
    $pending = $this->pendingDepth();
    if ($pending === 0) {
        // The memoized read can be a stale 0 or a memoized failed probe:
        // re-probe uncached before concluding the plan is drained (#287).
        $pending = $this->pendingDepth(fresh: true);
    }
    if ($pending > 0
        && ($reArms < self::MAX_ONE_SHOT_REARMS
            || $cleanExitsSinceReArm > 0
            || $pending < $lastReArmDepth)) {
        $reArms++;
        $lastReArmDepth = $pending;
        $cleanExitsSinceReArm = 0;
        $this->spawnInitialChildren($slots);

        continue;
    }
    if ($pending < 0 && $reArms < self::MAX_ONE_SHOT_REARMS) {
        // Every fresh lookup failed: inconclusive, retry within the same
        // bounded budget instead of reporting a drained plan.
        $reArms++;
        $cleanExitsSinceReArm = 0;

        continue;
    }

    break;
}
```

- [ ] **Step 8: Run the full supervisor + sampler suites** — `vendor/bin/pest tests/Feature/WorkerSupervisorIntegrationTest.php tests/Feature/QueueDepthSamplerTest.php` — expected PASS (the #269 and #272 tests must still pass).

- [ ] **Step 9: Full package suite** — `vendor/bin/pest` from `packages/laravel-queue` (Unit + Feature; Integration suites are CI-only) — expected green.

- [ ] **Step 10: CHANGELOG** — root + package, new `## [Unreleased]` section at top:

```markdown
## [Unreleased]

### Fixed

- `rabbit-rs:work --once` no longer ends its drain on a memoized stale reading (issue #287): the depth sampler's 2 s window (failures included) could report a cached 0 to the final drain check while the broker still held work — the exit decision now re-probes uncached once before concluding, and a fully failed fresh probe retries within the existing re-arm budget instead of reporting a drained plan.
```

- [ ] **Step 11: Commit + push + PR**

```bash
git checkout -b fix/once-fresh-exit origin/main   # (already on it inside the worktree)
git add -A
git commit -m "fix(laravel): re-probe the queue depth uncached before the once-mode drain decision"
git push -u origin fix/once-fresh-exit
gh pr create --title "fix(laravel): re-probe the queue depth uncached before the once-mode drain decision" --body "Closes #287"
```

---

## Task 2 (PR D, #290): surface the pool stats counters in PHP

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php` (add public `stats()` near `probeTurn()`, :574-589)
- Modify: `packages/laravel-queue/src/Console/RabbitMqStatusCommand.php` (`displayHuman`, :180-232 — `returns_total` printed at :195 without `dropped_publications_total`)
- Test: `packages/laravel-queue/tests/Unit/RabbitMqQueueStatsTest.php` (new — check `tests/bootstrap.php`'s native fake for the `stats()` shape first)
- Test: `packages/laravel-queue/tests/Integration/PublishErrorSurfacingTest.php` (extend — it already asserts `$this->pool->stats()['returns_total']` at :68/:133)

**Interfaces:**
- Produces: `RabbitMqQueue::stats(): array<string, int>` — passthrough of the native `Pool::stats()` hash table (`deliveries_total`, `acks_total`, `rejects_total`, `returns_total`, `dropped_publications_total`, `dropped_error_records_total`, …). Empty array when no pool is bound.
- Consumes: native counters already exist — `returns_total` (`crates/rabbit-rs-core/src/metrics.rs:114`, recorded at `publisher/actor.rs:918`), `dropped_publications_total` (`crates/rabbit-rs-php/src/classes/publish_buffer.rs:94`), both already emitted by `Pool::stats()` (`crates/rabbit-rs-php/src/classes/pool.rs:297-329`, stub shape at `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php:423-446`). **No Rust changes.**

**Context for the implementer:** issue #290's native half is already done; the gap is that Laravel userland cannot reach the counters (no public `stats()` on `RabbitMqQueue` — only the internal `probeTurn()` reads three delivery counters) and `rabbit-rs:status` shows `returns_total` but not drops. The doctor deliberately does NOT read process-local stats (one-shot CLI — see `RabbitMqDoctorCommand.php:250-258`); do not add a doctor panel for them.

- [ ] **Step 1: Write the failing unit test** — new `tests/Unit/RabbitMqQueueStatsTest.php`. First read `tests/bootstrap.php` to see the native fake's `Pool::stats()` shape (extend the fake if it does not provide a `stats()` hash table), then copy the construction of the wired `RabbitMqQueue` from the closest existing unit test in `tests/Unit/` and add:

```php
it('exposes the native pool stats including the return and drop counters', function () {
    $queue = /* the construction copied from the closest existing unit test */;

    $stats = $queue->stats();

    expect($stats)->toBeArray()
        ->and($stats['returns_total'])->toBeInt()
        ->and($stats['dropped_publications_total'])->toBeInt()
        ->and($stats['deliveries_total'])->toBeInt();
});
```

- [ ] **Step 2: Run it** — `vendor/bin/pest tests/Unit/RabbitMqQueueStatsTest.php` — expected FAIL (`stats()` does not exist on `RabbitMqQueue`).

- [ ] **Step 3: Implement** — in `RabbitMqQueue.php`:

```php
/**
 * Process-local native pool metrics for observability: delivery/ack/reject
 * totals plus the publish-outcome counters — returns_total (mandatory
 * publications returned as unroutable) and dropped_publications_total
 * (publications dropped on a closed client, never handed off, never
 * returned). These are per-process; a short-lived process takes them to
 * the grave, so drain them before exit when they matter.
 *
 * @return array<string, int>
 */
public function stats(): array
{
    if ($this->pool === null) {
        return [];
    }

    $stats = $this->pool->stats();

    return is_array($stats) ? $stats : [];
}
```

(Confirm the pool property name — `probeTurn()` at :581 uses `$this->pool->stats()`.)

- [ ] **Step 4: Run the unit test** — expected PASS.

- [ ] **Step 5: Extend the integration pin** — in `PublishErrorSurfacingTest.php`, next to the existing `returns_total` assertion (:68 or :133):

```php
expect($this->queue->stats()['returns_total'])->toBeGreaterThan(0)
    ->and($this->queue->stats()['dropped_publications_total'])->toBe(0);
```

- [ ] **Step 6: Status command display** — in `RabbitMqStatusCommand::displayHuman()` around :195, print the drop counter beside the return counter and flag non-zero drops:

```php
$dropped = (int) ($stats['dropped_publications_total'] ?? 0);
// existing returns_total line gains the dropped counter, e.g.:
//   returns_total: 2  dropped_publications_total: 0
if ($dropped > 0) {
    $this->components->warn("dropped_publications_total is {$dropped} — publications were dropped on a closed client; drainPublishErrors()/safe mode surface the details");
}
```

(Match the file's existing output style — read `displayHuman()` first; if it uses `$this->line()`/table rendering rather than components, follow that.)

- [ ] **Step 7: Full package suite** — `vendor/bin/pest` — expected green.

- [ ] **Step 8: CHANGELOG** — append under `## [Unreleased]` → `### Added` (create the section if PR A has not merged yet):

```markdown
- `RabbitMqQueue::stats()` exposes the process-local native pool counters to userland — including `returns_total` (unroutable mandatory publications) and `dropped_publications_total` (publications dropped on a closed client) — and `rabbit-rs:status` now reports the drop counter and warns when it is non-zero (issue #290).
```

- [ ] **Step 9: Commit + push + PR**

```bash
git add -A
git commit -m "feat(laravel): expose the native pool stats and the drop counter through RabbitMqQueue::stats() and rabbit-rs:status"
git push -u origin feat/pool-stats-surface
gh pr create --title "feat(laravel): expose the native pool stats and the drop counter" --body "Closes #290"
```

---

## Task 3 (PR B, #288): dedicated canary DLQ with tiered verdicts and teardown

**Files:**
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php` (canary methods: `deadLetterCanary()` :148-240, `assertCanaryOnDlq()` :259-293, `pullFromQueue()` :302-316; add canary-DLQ declare/bind/teardown helpers)
- Modify: `packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php` (ok-message text only if changed, :401-413)
- Test: `packages/laravel-queue/tests/Integration/DoctorDlxCanaryTest.php` (update :58-131 + add purge/teardown case)
- Test: `packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php` (text assertions only if the ok message changes)

**Interfaces:**
- Produces: canary DLQ name `rabbit-rs.canary.<first 16 hex of sha256(broker|dlx|routing_key)>` — doctor-owned, declared on demand, deleted after each check.
- Produces: three-tier verdict in `deadLetterCanary()`:
  1. canary found in the canary DLQ **and** in the configured DLQ's first 100 → `[ok]` (full proof: queue args → DLX → binding → configured DLQ);
  2. canary found in the canary DLQ but deeper than the configured DLQ's 100-message scan window → `CanaryInconclusiveException` (`[warn]` — wiring verified, configured-DLQ delivery unverified, foreign count reported);
  3. canary never reaches the canary DLQ → `RuntimeException` (`[fail]` — the wiring is broken; strictly stronger than today's inconclusive-on-backlog).

**Context for the implementer:** today the canary is scanned for only inside the *configured* DLQ with a 100-message `ack_requeue_true` window (`CANARY_DLQ_SCAN_WINDOW = 100`, DoctorProbe.php:31) and deliberately left behind (:247-250) — so canaries + foreign messages accumulate forever and every long-lived broker degrades to permanent `inconclusive` (issue #288; the playground hit it at 188 messages). The management API is already required for this check (`RabbitMqDoctorCommand.php:377-379` guards on `$managementUsable`). A direct exchange routes a copy of every matching message to **every** bound queue, so a second queue bound to the configured DLX with the dead-letter routing key receives the canary too — and because the doctor purges+deletes it after each run, its window can never be walled off. The configured-DLQ scan stays as tier 1/2 evidence (it is the proof the *configured* DLQ works); the canary DLQ makes tier 3 a decisive fail instead of an inconclusive. Queue-create/bind/delete patterns live in `tests/Pest.php:139-150` (`declareQueue` = `PUT /api/queues/{vhost}/{name}`, `deleteQueue`); the doctor's HTTP style is inline `Http::withBasicAuth(...)` (DoctorProbe.php:304). No topology-compiler changes: `rabbit-rs:topology` verify/`--fix` only know subscription queues + the configured dead-letter objects (`RabbitMqTopologyCommand.php:102-126, 292-319`) and perform no unexpected-object scan, so a transient canary DLQ cannot trip them — and because it is deleted after each run, nothing lingers anyway.

- [ ] **Step 1: Write the failing integration tests** — in `tests/Integration/DoctorDlxCanaryTest.php`:

New case — the 110-foreign-backlog test (:83-95) flips from inconclusive to ok-with-canary-DLQ (keep the old test but re-assert; it currently expects `dead-letter canary inconclusive`):

```php
it('verifies the wiring through a doctor-owned canary DLQ even when the configured DLQ backlog outgrows the scan window', function () {
    declareCanaryTopology($this->connectionName, $this->config);
    seedForeignDeadLetters($this->dlq, 110);

    $output = Artisan::output();
    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);
    $output = Artisan::output();

    expect($output)->toContain('dead-letter canary: delivered, rejected, and received on the configured DLQ')
        ->and(Artisan::output())->not->toContain('dead-letter canary failed');
});
```

(Adapt to the file's existing Artisan call + output-capture pattern; the intent: 110 foreign dead-letters ahead of the canary in the configured DLQ must no longer produce `inconclusive` — the canary is found via the canary DLQ.)

New case — teardown:

```php
it('purges and deletes its canary DLQ after the check so nothing accumulates', function () {
    declareCanaryTopology($this->connectionName, $this->config);

    Artisan::call('rabbit-rs:doctor', ['--connection' => [$this->connectionName]]);

    $canaryQueues = array_values(array_filter(
        managementRequest('GET', "api/queues")->json(),
        static fn (array $queue): bool => str_starts_with((string) $queue['name'], 'rabbit-rs.canary.'),
    ));
    expect($canaryQueues)->toBe([]);
});
```

New case — broken wiring stays a hard fail with a stronger cause (adapt the existing broken-wiring test at :117-131): expect `dead-letter canary failed` **and** `never reached the DLX`.

Also update the end-to-end ok test (:58-67) to the new ok text (`received on the configured DLQ`).

- [ ] **Step 2: Run them** — `vendor/bin/pest tests/Integration/DoctorDlxCanaryTest.php` (needs the live broker; locally use the playground broker or mark for CI) — expected FAIL (no canary DLQ logic yet).

- [ ] **Step 3: Implement in `DoctorProbe.php`** —

Add the prefix + name helper:

```php
private const CANARY_DLQ_PREFIX = 'rabbit-rs.canary.';

/**
 * Doctor-owned DLQ for the canary: bound to the configured DLX with the
 * dead-letter routing key, so a copy of every dead-lettered message lands
 * here. The doctor declares it before the check and purges+deletes it
 * after, so canaries and foreign traffic can never wall off the verdict
 * (issue #288).
 */
private function canaryDlqName(string $broker, string $exchange, string $routingKey): string
{
    return self::CANARY_DLQ_PREFIX.substr(hash('sha256', $broker.'|'.$exchange.'|'.$routingKey), 0, 16);
}
```

Declare + bind (management API, doctor's existing inline-Http style):

```php
private function declareCanaryDlq(
    string $base,
    string $vhost,
    string $canaryDlq,
    string $dlx,
    string $routingKey,
    string $username,
    string $password,
): void {
    Http::withBasicAuth($username, $password)
        ->put("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq), [
            'durable' => true,
            'auto_delete' => false,
            'arguments' => ['x-queue-type' => 'quorum'],
        ])->throw();
    Http::withBasicAuth($username, $password)
        ->post("{$base}/api/exchanges/".rawurlencode($vhost).'/'.rawurlencode($dlx).'/bindings', [
            'routing_key' => $routingKey,
        ])->throw();
}

private function teardownCanaryDlq(string $base, string $vhost, string $canaryDlq, string $username, string $password): void
{
    try {
        Http::withBasicAuth($username, $password)
            ->delete("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq).'/contents')->throw();
    } catch (\Throwable) {
        // best-effort hygiene; never mask the check's verdict
    }
    try {
        Http::withBasicAuth($username, $password)
            ->delete("{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($canaryDlq))->throw();
    } catch (\Throwable) {
        // same
    }
}
```

Rewire `deadLetterCanary()` (:148-240): compute `$canaryDlq = $this->canaryDlqName(...)` early; `declareCanaryDlq(...)` right after the main-queue routing values are known; wrap the reject+scan sequence in try/finally with `teardownCanaryDlq(...)` in the finally. Replace the `assertCanaryOnDlq()` body:

```php
private function assertCanaryOnDlq(
    string $base,
    string $vhost,
    string $username,
    string $password,
    string $messageId,
    string $configuredDlq,
    string $canaryDlqUrl,
): void {
    $configuredDlqUrl = "{$base}/api/queues/".rawurlencode($vhost).'/'.rawurlencode($configuredDlq).'/get';

    // Tier 1: the canary must reach the doctor-owned canary DLQ — the DLX
    // is alive and routed. This queue is purged+deleted each run, so the
    // 100-message window can never be walled off by a backlog (#288).
    // Reuse the existing 30-attempt x 300 ms poll loop; on exhaustion:
    throw new RuntimeException("dead-lettered canary never reached the DLX — the dead-letter wiring is broken (the canary DLQ was empty), so dead-lettered messages would vanish");

    // Tier 2: configured-DLQ evidence (single 100-message window, requeued).
    $foreign = 0;
    $messages = $this->pullFromQueue($configuredDlqUrl, $username, $password, 'ack_requeue_true', self::CANARY_DLQ_SCAN_WINDOW);
    foreach ($messages as $message) {
        $candidate = is_array($message['properties'] ?? null) ? (string) ($message['properties']['message_id'] ?? '') : '';
        if ($candidate === $messageId) {
            return; // [ok] full chain proven on the configured DLQ
        }
        $foreign++;
    }

    throw new CanaryInconclusiveException(
        "wiring verified through the canary DLQ, but the canary sits deeper than the ".self::CANARY_DLQ_SCAN_WINDOW
        ."-message scan window of the configured DLQ ({$foreign} foreign/stale messages at its head) — dead-letter delivery to the configured DLQ unverified"
    );
}
```

(The `$canaryDlqUrl` is built by the caller from the same URL pattern; the poll loop, the `pullFromQueue` reuse, and the tier order are the contract — write the final control flow so the poll loop's exhaustion throw sits before the tier-2 scan, not literally as sketched.)

And the decisive branch (canary DLQ): reuse the existing 30×300 ms poll loop (:271-293) but on exhaustion throw

```php
throw new RuntimeException("dead-lettered canary never reached the DLX — the dead-letter wiring is broken (the canary DLQ was empty), so dead-lettered messages would vanish");
```

instead of the old whole-queue-scanned message. Keep the competing-consumer and not-consumed-in-10 s branches (:215-229) untouched. Update the ok message at `RabbitMqDoctorCommand.php:413` to `dead-letter canary: delivered, rejected, and received on the configured DLQ`.

- [ ] **Step 4: Run the integration suite** — expected PASS; the existing "40 foreign messages" test (:69-81) must still pass (canary at position 41 ≤ window → tier 1 ok).

- [ ] **Step 5: Unit text check** — `grep -rn "received on the DLQ" packages/laravel-queue/tests/` — update the unit assertion at `RabbitMqDoctorCommandTest.php:346`-region if it pins the old ok text; re-run `vendor/bin/pest tests/Unit/Console/RabbitMqDoctorCommandTest.php`.

- [ ] **Step 6: Full package suite** — `vendor/bin/pest`.

- [ ] **Step 7: CHANGELOG** — under `## [Unreleased]` → `### Fixed`:

```markdown
- The doctor's dead-letter canary no longer self-sandbags behind DLQ backlog (issue #288): the check now also binds a doctor-owned `rabbit-rs.canary.*` DLQ to the configured dead-letter exchange, purges and deletes it after every run, and tiers the verdict — found on the configured DLQ → ok, found only in the canary DLQ (configured DLQ backlog deeper than the 100-message scan window, foreign count reported) → warn, never reaching the canary DLQ → hard fail.
```

- [ ] **Step 8: Commit + push + PR**

```bash
git add -A
git commit -m "fix(laravel): doctor canary verifies through a doctor-owned DLQ with tiered verdicts"
git push -u origin fix/doctor-canary-dlq
gh pr create --title "fix(laravel): doctor canary verifies through a doctor-owned DLQ with tiered verdicts" --body "Closes #288"
```

---

## Task 4 (PR C, #285): typed coordinator errors through admin/consumer acquisition

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs` — consumer acquisition (:394-411) and admin-channel acquisition (:538-554)
- Test: existing Rust suites under `crates/rabbit-rs-core/tests/` (locate the closest client/coordinator test module first — `grep -rn "admin_channel\|did not become ready" crates/rabbit-rs-core/tests/`)

**Interfaces:**
- Preserves (PHP string contracts — do not break): `str_contains($error, 'did not become ready within')` (`packages/laravel-queue/src/Console/RabbitMqTopologyCommand.php:384-387`) and `str_contains($error, 'NOT-FOUND')` (:116).
- Produces: when the underlying coordinator/transport error is known, its typed Display is **appended** to the fabricated timeout text instead of being discarded; when the pool has already published `ConnectionState::FailedPermanent { reason }`, admin operations fail **immediately** with `broker connection failed permanently: {reason}` (no wait, no fabricated text).

**Context for the implementer:** the pipeline is `lapin::Error → TransportError(kind) → ClientError(kind) → PHP exception (message-only)`. No `lapin::Error` object escapes, but the **raw lapin message text** (`invalid connection state: Closed`) does — because `client.rs` discards typed errors with `.ok()` at :396 (consumer) and :543 (admin channel) and fabricates `"… did not become ready …"` text on timeout. The permanent-failure reason is already published on the watch channel (`connection_actor.rs:521-526` → `publish_permanent_failure`) and consumed at `client.rs:584-588`/`:741-748` — reuse that path. The 403→`FailedPermanent` classification itself is **by design** (red-team audit `docs/audit/2026-08-31-red-team-audit.md:778`): do NOT add retries for it; the deliverable is legibility (the verify probe should surface "declare refused: ACCESS_REFUSED …", not "invalid connection state: Closed").

- [ ] **Step 0: Read the exact current blocks** — `client.rs:380-415` (consumer) and `client.rs:530-560` (admin channel) plus how the client accesses the connection-state watch (`client.rs:584-588`, `:741-748`).

- [ ] **Step 1: Write the failing Rust test** — in the closest existing client test module (follow its harness/broker setup; if the suite requires a live broker harness, follow the pattern of `crates/rabbit-rs-core/tests/metrics.rs`):

```rust
#[tokio::test]
async fn admin_operations_surface_the_permanent_failure_reason_instead_of_raw_state_text() {
    // Harness: bring the client up against the test broker with a topology
    // the broker refuses (the same shape TopologyFixVerificationTest races),
    // wait for the pool to publish FailedPermanent, then:
    let error = client.admin_channel(broker, Duration::from_secs(2))
        .await
        .expect_err("a permanently failed pool must not yield an admin channel");

    assert!(
        error.to_string().contains("failed permanently"),
        "expected the permanent-failure reason, got: {error}"
    );
    assert!(
        !error.to_string().contains("invalid connection state"),
        "raw lapin state text leaked: {error}"
    );
}
```

And for the consumer path: a coordinator whose profile establishment fails with a typed error must produce a timeout message **containing** the typed cause after the existing `"did not become ready within"` substring.

- [ ] **Step 2: Run it** — `rtk cargo nextest run -p rabbit-rs-core <test>` — expected FAIL (current text is the fabricated timeout / raw state message).

- [ ] **Step 3: Implement** —
Admin channel (:538-554): first consult the published connection state; on `FailedPermanent { reason }` return `ClientError::transport(&format!("broker connection failed permanently: {reason}"))` immediately. Otherwise replace the `.ok()` discard: on `Err(error)` from `coordinator.admin_channel()`, build `"broker '{broker}' did not become ready for the admin operation within {wait_timeout:?}: {error}"` (keep the existing prefix verbatim for PHP string contracts).
Consumer acquisition (:394-411): same pattern — on `Err(error)`, append `": {error}"` after the existing `"consumer profile '{profile}' did not become ready within {wait_timeout:?}"` text (prefix preserved, cause added).

- [ ] **Step 4: Run it + the workspace** — `rtk cargo fmt --all && rtk cargo nextest run --workspace --all-targets --no-fail-fast` — expected green (the PHP-side relaxed matcher from #286 still passes because both accepted branches remain).

- [ ] **Step 5: Quality gate** — `scripts/check.sh` — expected green.

- [ ] **Step 6: CHANGELOG** — under `## [Unreleased]` → `### Fixed`:

```markdown
- Admin operations and consumer acquisition no longer discard the coordinator's typed errors (issue #285): a permanently failed pool fails admin calls immediately with its published permanent-failure reason instead of waiting out a timeout and surfacing raw lapin state text (`invalid connection state: Closed`), and timed-out consumer establishment now reports the underlying coordinator error after the existing readiness message.
```

- [ ] **Step 7: Commit + push + PR**

```bash
git add -A
git commit -m "fix(core): surface typed coordinator errors through admin and consumer acquisition"
git push -u origin fix/admin-error-classification
gh pr create --title "fix(core): surface typed coordinator errors through admin and consumer acquisition" --body "Closes #285"
```

---

## Task 5 (after all four merge): integration validation + issue/doc close-out

**Files:** none in the monorepo — validation runs in the playground (`/Users/zacharyvolpi/dev/perso/rabbit-rs-playground`).

- [ ] **Step 1: Merge order** — merge A → D → B → C (squash via PR). Rebase B onto main after A+D; rebase C onto main after B (DoctorProbe.php touch point).
- [ ] **Step 2: Playground roast** — upgrade the playground to the new dist (`pie install goopil/rabbit-rs-native:^0.3.5` once tagged, or the branch artifact), then:
  - Drain validation: dispatch 215 jobs, run `rabbit-rs:work --once` — expect convergence in ≤2 passes (the #287 regression is gone; 0.3.3 baseline was 208/0).
  - Doctor validation: run `rabbit-rs:doctor` 3× — expect stable `[ok]` canary verdicts, no `rabbit-rs.canary.*` queue left behind between runs, no accumulation in the configured DLQ.
  - Topology flake scenario: run `rabbit-rs:topology --verify` (or the CI Coverage scenario) — raced failures must now read "failed permanently: …" instead of "invalid connection state: Closed".
  - `rabbit-rs:status` shows `dropped_publications_total`.
- [ ] **Step 3: Close-out** — verify issues #287/#288/#285/#290 auto-closed by the squash merges; update the playground's `docs/upstream-rabbit-rs-laravel.md` ("Current status — post-v0.3.4" section) to move the four items to Fixed with verification notes.
