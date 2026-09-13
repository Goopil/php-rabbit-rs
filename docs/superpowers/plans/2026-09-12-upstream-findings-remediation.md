# Upstream findings remediation — doctor observability, DLX canary, delay docs, flush-timer measurement

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the four still-open items from the external playground's 0.2.2 report (reconciled in #253): broker-side unroutable-publish observability in `rabbit-rs:doctor` (#252), a dead-letter canary in the doctor (#219), the missing quorum-TTL ceiling documentation (#254), and a decision-grade re-measurement of the publish flush-timer latency (#255).

**Architecture:** Streams A and B extend the doctor's check pipeline (`RabbitMqDoctorCommand` + `DoctorProbe`), A with a management-API read of the publish exchange's `return_unroutable` counter, B with an end-to-end consume→reject→verify probe through the existing transient-pool machinery. Stream C is docs-only. Stream D is a measurement script plus a decision protocol — no product code.

**Tech Stack:** PHP 8.x, Laravel package (`packages/laravel-queue`), Pest tests, ext-rabbit_rs (Rust cdylib), RabbitMQ management HTTP API.

## Global Constraints

- Rust is pinned to 1.96.0, edition 2024; `#![forbid(unsafe_code)]` is inviolable.
- All repository artifacts (code comments, docs, commits) in English.
- Laravel Unit/Feature tests must run **without** the extension (fake `DoctorProbe`, `Http::fake()`); Integration tests run with the extension against the lab broker.
- PHP tests use Pest, not PHPUnit.
- Follow TDD: failing test first, observe it fail, minimal implementation, rerun.
- Before claiming completion run the full gate: `rtk ./scripts/check.sh` (fmt + clippy + nextest + composer validate). For PHP-only changes, at minimum: `./scripts/test-laravel.sh` (Unit + Feature) and `rtk composer validate --strict`.
- Commits are conventional and scoped (`fix(laravel): …`, `docs(laravel): …`, `test(ext): …`); never include `.air/`, IDE metadata, or unrelated files.
- The delivery contract is at-least-once: never weaken blind-mode semantics (fire-and-forget by contract) while adding observability.

## Parallelism map

| Stream | Issue | Files touched | Parallel with |
|---|---|---|---|
| A — doctor publish outcomes | #252 | `RabbitMqDoctorCommand.php` + its Unit test | B (⚠ see below), C, D |
| B — doctor DLX canary | #219 | `DoctorProbe.php`, `RabbitMqDoctorCommand.php` + tests | C, D |
| C — quorum-TTL ceiling docs | #254 | `packages/laravel-queue/docs/reference.md`, `packages/laravel-queue/CHANGELOG.md` | A, B, D |
| D — flush-timer latency measurement | #255 | `scripts/flush-latency-probe.php`, `scripts/flush-latency-probe.sh` | A, B, C |

⚠ **A and B both edit `RabbitMqDoctorCommand.php`.** They touch disjoint methods, but run them **sequentially in the order A → B** (or rebase B on A) to avoid merge friction. C and D are fully independent — start them immediately in parallel with A.

Reference state: `origin/main` @ `4db9434` (v0.3.1+). Local checkouts must be up to date before starting: `git fetch origin && git switch -c <branch> origin/main`.

---

### Task 1 (Stream C, issue #254): document the quorum-TTL lazy-release ceiling

**Files:**
- Modify: `packages/laravel-queue/docs/reference.md` (section "### Delay routing", ends before "### Topology and recovery" at line ~1108)
- Modify: `packages/laravel-queue/CHANGELOG.md` (Unreleased/docs section)

**Interfaces:**
- Consumes: nothing (docs-only).
- Produces: a "Release ceiling and idleness" subsection an operator can link from delay-related incidents.

- [ ] **Step 1: Add the subsection to the Delay routing section**

Insert at the end of `### Delay routing` (just before `### Topology and recovery`):

```markdown
#### Release ceiling and idleness

`later(N)` guarantees delivery **no earlier than** N seconds. It does not guarantee
delivery *at* N seconds: bucket queues are quorum queues whose `x-message-ttl` expiry
is lazy broker semantics. On an idle broker the sweep can lag the deadline by tens of
seconds — observed tails of 9 s on a 2 s delay and minutes on a 30 s delay while the
broker had no other traffic. The deadline is a floor with no ceiling.

Two mitigations exist, neither configurable into a hard bound:

- Live buckets are re-declared on a keep-alive schedule, which prompts the sweep.
  `queue_expiry_margin` (default 60 s) controls how long an idle bucket outlives its
  last use before it is expired and cleaned up.
- The `delay.mode=plugin` strategy is driven by the delayed-message plugin, not by the
  sweep, and does not exhibit this tail. Latency-sensitive delay workloads should
  either use the plugin or keep the broker non-idle.
```

- [ ] **Step 2: Add the CHANGELOG entry**

Under the unreleased section of `packages/laravel-queue/CHANGELOG.md`, add:

```markdown
- Documented the quorum-TTL release ceiling for delayed messages: `later(N)` is a
  floor, the broker's lazy TTL sweep gives the upper bound no guarantee on an idle
  broker; keep-alive redeclaration mitigates, `delay.mode=plugin` avoids it.
```

- [ ] **Step 3: Verify the docs build checks**

Run: `rtk composer validate --strict`
Expected: PASS (docs changes must not break the package manifest gate).

- [ ] **Step 4: Commit**

```bash
git add packages/laravel-queue/docs/reference.md packages/laravel-queue/CHANGELOG.md
git commit -m "docs(laravel): document the quorum-TTL lazy-release ceiling (#254)"
```

---

### Task 2 (Stream D, issue #255): measure the publish flush-timer latency against the 1 ms contract

**Files:**
- Create: `scripts/flush-latency-probe.php`
- Create: `scripts/flush-latency-probe.sh`

**Interfaces:**
- Consumes: `scripts/lib-extension.sh` helpers (`ext_php_cmd`, `ext_ensure_built`); the lab broker (`scripts/test-integration.sh` brings up the RabbitMQ lab); management API at `http://localhost:15672` (guest/guest, vhost `/`).
- Produces: a latency report (p50/p95/p99/max, cold and warm) pasted into #255, plus the decision (doc note vs follow-up implementation issue). **This task never modifies product code.**

- [ ] **Step 1: Write the probe script**

`scripts/flush-latency-probe.php` — standalone, reads `RABBIT_RS_PROBE_RUNS` (default 100), `RABBIT_RS_PROBE_QUEUE` (default `flush-latency-probe`):

```php
<?php

declare(strict_types=1);

// Lone-publish latency probe: publishes one message into an empty queue and
// polls the management API until the broker reports depth >= 1, measuring
// dispatch -> broker-visible latency. Cold mode recreates the Pool per run
// (worst case: timer not yet scheduled); warm mode reuses one pool.

$runs = max(1, (int) (getenv('RABBIT_RS_PROBE_RUNS') ?: 100));
$queue = getenv('RABBIT_RS_PROBE_QUEUE') ?: 'flush-latency-probe';
$mode = getenv('RABBIT_RS_PROBE_MODE') ?: 'cold'; // cold|warm
$management = getenv('RABBIT_RS_PROBE_MANAGEMENT') ?: 'http://guest:guest@localhost:15672';

$config = [
    'brokers' => [['name' => 'lab', 'hosts' => [['host' => 'localhost', 'port' => 5672]], 'vhost' => '/']],
    'workers' => [[
        'name' => 'probe',
        'subscriptions' => [['name' => 'probe', 'queue' => $queue, 'prefetch' => 10]],
    ]],
    'publisher' => ['safety' => 'unsafe', 'confirm_timeout' => 5000, 'flush_interval' => 1],
    'routes' => ['default' => ['exchange' => '', 'routing_key' => '{queue}']],
    'topology_mode' => 'declare',
];

$purge = function () use ($management, $queue): void {
    // Recreate the queue so every run starts from depth 0; declare mode
    // recreates it on the next publish.
    $http = curl_init("{$management}/api/queues/%2F/".rawurlencode($queue));
    curl_setopt_array($http, [CURLOPT_CUSTOMREQUEST => 'DELETE', CURLOPT_RETURNTRANSFER => true, CURLOPT_HTTPAUTH => CURLAUTH_BASIC, CURLOPT_USERPWD => 'guest:guest']);
    curl_exec($http);
    curl_close($http);
};

$depth = function () use ($management, $queue): int {
    $http = curl_init("{$management}/api/queues/%2F/".rawurlencode($queue));
    curl_setopt_array($http, [CURLOPT_RETURNTRANSFER => true, CURLOPT_HTTPAUTH => CURLAUTH_BASIC, CURLOPT_USERPWD => 'guest:guest']);
    $body = curl_exec($http);
    curl_close($http);

    return (int) ((json_decode((string) $body, true) ?? [])['messages_ready'] ?? 0);
};

$pool = $mode === 'warm' ? new Goopil\RabbitRs\Pool($config) : null;
$latencies = [];

for ($i = 0; $i < $runs; $i++) {
    $purge();
    $pool ??= new Goopil\RabbitRs\Pool($config);
    $start = hrtime(true);
    $pool->publish([
        'broker' => 'lab',
        'exchange' => '',
        'routing_key' => $queue,
        'payload' => "latency-probe-{$i}",
        'message_id' => 'probe-'.uniqid(),
        'timeout_ms' => 5000,
    ]);
    $deadline = $start + 60_000_000_000; // 60 s ceiling; a miss is recorded as 60000
    do {
        $visible = $depth() >= 1;
        if ($visible) {
            break;
        }
        usleep(2000); // 2 ms poll step — management API resolution
    } while (hrtime(true) < $deadline);
    $latencies[] = $visible ? (hrtime(true) - $start) / 1e6 : 60_000.0;
    if ($mode === 'cold') {
        $pool->close();
        $pool = null;
    }
}
$pool?->close();

sort($latencies);
$pick = static fn (float $p): float => $latencies[min(count($latencies) - 1, (int) floor($p * count($latencies)))];
printf(
    "mode=%s runs=%d p50=%.1fms p95=%.1fms p99=%.1fms max=%.1fms misses=%d\n",
    $mode,
    $runs,
    $pick(0.50),
    $pick(0.95),
    $pick(0.99),
    end($latencies),
    count(array_filter($latencies, static fn (float $l): bool => $l >= 60_000.0)),
);
```

- [ ] **Step 2: Write the wrapper script**

`scripts/flush-latency-probe.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib-extension.sh
source "${SCRIPT_DIR}/lib-extension.sh"
ext_ensure_built
php_ini="-d extension=$(ext_artifact_path)"
echo "== cold pool =="
RABBIT_RS_PROBE_MODE=cold RABBIT_RS_PROBE_RUNS="${1:-100}" php $php_ini "${SCRIPT_DIR}/flush-latency-probe.php"
echo "== warm pool =="
RABBIT_RS_PROBE_MODE=warm RABBIT_RS_PROBE_RUNS="${1:-100}" php $php_ini "${SCRIPT_DIR}/flush-latency-probe.php"
```

Make it executable: `chmod +x scripts/flush-latency-probe.sh`.

- [ ] **Step 3: Run against the lab broker**

Run: `./scripts/test-integration.sh --setup-only || true` (ensure the lab stack is up; or start the lab per that script's docs), then:

```bash
./scripts/flush-latency-probe.sh 200
```

Expected: two report lines. Record them verbatim.

- [ ] **Step 4: Decide per the protocol in #255**

- p99 < 100 ms → the tail is gone on main; add the measured numbers as a latency note in `packages/laravel-queue/docs/reference.md` (publisher `flush_interval` paragraph) and close #255.
- Multi-second outliers reproduce → file the follow-up implementation issue with both report lines attached (candidate directions listed in #255) and close #255 as superseded by it.

- [ ] **Step 5: Commit**

```bash
git add scripts/flush-latency-probe.php scripts/flush-latency-probe.sh
git commit -m "test: add the publish flush-timer latency probe (#255)"
```

(If Step 4 produced a doc note, include it in a separate `docs(laravel):` commit.)

---

### Task 3 (Stream A, issue #252): doctor publish-outcome check — broker-side unroutable visibility

**Files:**
- Modify: `packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php`
- Test: `packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`

**Interfaces:**
- Consumes: `checkManagement()` gate (refactored to return `bool`), `compiled['routes']['default']['exchange']`, `compiled['native']['brokers'][0]['vhost']`, `compiled['publisher']['safety']`, the `emit()` helper, the test helpers `bindFakeProbe()` / `doctorConnection()`.
- Produces: `checkPublishOutcomes(array $compiled, array $config, bool $managementUsable): void` — Task 4 (canary) is independent of this signature but lands after it in the same file.

- [ ] **Step 1: Write the failing tests**

Append to `packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`:

```php
describe('rabbit-rs:doctor publish outcomes', function () {
    const EXCHANGE_URL = 'http://localhost:15672/api/exchanges/%2F/laravel.jobs';

    function doctorHttpFake(int $returned): void
    {
        Http::fake([
            '*/api/overview' => Http::response(['listening_port' => 5672], 200),
            '*/api/exchanges/*' => Http::response([
                'name' => 'laravel.jobs',
                'message_stats' => ['return_unroutable' => $returned],
            ], 200),
        ]);
    }

    function doctorExchangeConnection(): void
    {
        doctorConnection('rabbitmq', [
            'exchange' => 'laravel.jobs',
            'management_url' => 'http://localhost:15672',
            'safety' => 'safe',
        ]);
    }

    it('fails when safe mode has unroutable publishes on the publish exchange', function () {
        bindFakeProbe($this->app);
        doctorExchangeConnection();
        doctorHttpFake(3);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain('3 unroutable publish(es)')
            ->and($output)->toContain('fix the exchange')
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(1);
    });

    it('warns for unroutable publishes under a fire-and-forget safety mode', function () {
        bindFakeProbe($this->app);
        doctorConnection('rabbitmq', [
            'exchange' => 'laravel.jobs',
            'management_url' => 'http://localhost:15672',
            'safety' => 'blind',
        ]);
        doctorHttpFake(2);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain('2 unroutable publish(es)')
            ->and($output)->toContain('fire-and-forget');
    });

    it('reports ok when the broker recorded no unroutable publishes', function () {
        bindFakeProbe($this->app);
        doctorExchangeConnection();
        doctorHttpFake(0);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain("no unroutable publishes on exchange 'laravel.jobs'")
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(0);
    });

    it('skips the check when no management_url is configured', function () {
        bindFakeProbe($this->app);
        doctorConnection('rabbitmq', ['exchange' => 'laravel.jobs']);
        Http::fake(); // any call would fail; the check must not run at all

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->not->toContain('unroutable');
    });

    it('warns without failing when the exchange is missing from the management api', function () {
        bindFakeProbe($this->app);
        doctorExchangeConnection();
        Http::fake([
            '*/api/overview' => Http::response([], 200),
            '*/api/exchanges/*' => Http::response([], 404),
        ]);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->not->toContain('unroutable');
            // a 404 on the exchange is topology's business, not outcomes'
    });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `./scripts/test-laravel.sh` (Unit group) or `sail test --group upstream` equivalent: `php artisan test packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`
Expected: the five new cases FAIL (no "unroutable" output exists yet; the safe-mode case fails on exit code 1).

- [ ] **Step 3: Implement `checkPublishOutcomes`**

In `RabbitMqDoctorCommand.php`:

1. Change `checkManagement(array $config): void` to `checkManagement(array $config): bool` — return `false` at each early return, `true` after the `emit('ok', 'management api reachable')`.
2. In `doctorConnection()`, after `checkManagement`:

```php
$managementUsable = $this->checkManagement($config);
$this->checkPublishOutcomes($compiled, $config, $managementUsable);
```

3. Add the method (after `checkTopology`):

```php
/**
 * Reports broker-truth unroutable publishes on the connection's publish
 * exchange, read from the management API. The pool's process-local
 * `returns_total`/`dropped_publications_total` counters cannot answer this
 * in a one-shot CLI (its own pool publishes nothing); the exchange counter
 * is cross-process and survives process exit.
 *
 * @param  array<string, mixed>  $compiled
 * @param  array<string, mixed>  $config
 */
private function checkPublishOutcomes(array $compiled, array $config, bool $managementUsable): void
{
    if (! $managementUsable) {
        return;
    }

    $exchange = $compiled['routes']['default']['exchange'] ?? '';
    if (! is_string($exchange) || $exchange === '') {
        return; // the default exchange cannot report a publish-side exchange counter
    }

    $vhost = $compiled['native']['brokers'][0]['vhost'] ?? '/';
    $username = is_string($config['username'] ?? null) ? $config['username'] : 'guest';
    $password = is_string($config['password'] ?? null) ? $config['password'] : 'guest';

    try {
        $response = Http::withBasicAuth($username, $password)
            ->timeout(5)
            ->acceptJson()
            ->get(rtrim(trim((string) $config['management_url']), '/').'/api/exchanges/'.rawurlencode((string) $vhost).'/'.rawurlencode($exchange));
    } catch (\Throwable $e) {
        $this->emit('warn', 'publish outcomes not verified: management api unreachable — '.$e->getMessage());

        return;
    }

    if (! $response->successful()) {
        return; // a missing exchange is the topology check's finding, not an outcome
    }

    $returned = (int) ($response->json('message_stats.return_unroutable') ?? 0);
    if ($returned === 0) {
        $this->emit('ok', "no unroutable publishes on exchange '{$exchange}'");

        return;
    }

    $message = sprintf("%d unroutable publish(es) returned by the broker on exchange '%s'", $returned, $exchange);
    if (($compiled['publisher']['safety'] ?? 'safe') === 'safe') {
        $this->emit('fail', $message.' — safe mode published them as lost; fix the exchange→queue binding');

        return;
    }

    $this->emit('warn', $message.' — the safety mode is fire-and-forget: returns are silent by contract');
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: the same command as Step 2.
Expected: all five new cases PASS, and the pre-existing doctor suite stays green.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php
git commit -m "feat(laravel): report broker-side unroutable publishes in rabbit-rs:doctor (#252)"
```

---

### Task 4 (Stream B, issue #219): doctor DLX canary — end-to-end dead-letter proof

**Ordering:** land **after Task 3** (same file; disjoint methods, sequential keeps conflicts away).

**Files:**
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php` (new `deadLetterCanary()`)
- Modify: `packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php` (new `checkDeadLetterCanary()` wired into `doctorConnection()`)
- Test (unit, no ext): `packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`
- Test (integration, ext + lab): `packages/laravel-queue/tests/Integration/DoctorDlxCanaryTest.php`

**Interfaces:**
- Consumes: `compiled['topology']['dead_letter']` (`['exchange' =>, 'queue' =>]` or `null`), `compiled['routes']['default']` (`['exchange' =>, 'routing_key' =>]`, `{queue}` placeholder), `compiled['native']['workers'][0]['name']` (worker profile), the `emit()` helper, `probePool()` transient-pool pattern, `Pool::publish/publishBatch`, `Consumer::next()`, `Delivery::metadata()['message_id']`, `Delivery::reject(bool)`, management API `POST /api/queues/{vhost}/{queue}/get`.
- Produces: `DoctorProbe::deadLetterCanary(array $nativeConfig, string $broker, string $exchange, string $routingKey, string $dlq, string $messageId): ?string` (null = canary delivered).

- [ ] **Step 1: Write the failing unit tests**

Append to `RabbitMqDoctorCommandTest.php`:

```php
describe('rabbit-rs:doctor dead-letter canary', function () {
    function doctorDeadLetterConnection(): void
    {
        doctorConnection('rabbitmq', [
            'exchange' => 'laravel.jobs',
            'management_url' => 'http://localhost:15672',
            'dead_letter' => ['exchange' => 'laravel.dlx', 'queue' => 'laravel.dead'],
            'topology_mode' => 'declare',
        ]);
    }

    it('reports the canary as ok when the probe reports success', function () {
        bindFakeProbe($this->app, canaryError: null);
        doctorDeadLetterConnection();
        Http::fake(['*/api/overview' => Http::response([], 200)]);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain('dead-letter canary: delivered')
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(0);
    });

    it('fails when the canary does not land in the dead-letter queue', function () {
        bindFakeProbe($this->app, canaryError: 'canary message not found in DLQ within 5s');
        doctorDeadLetterConnection();
        Http::fake(['*/api/overview' => Http::response([], 200)]);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->toContain('dead-letter canary failed')
            ->and(Artisan::call('rabbit-rs:doctor'))->toBe(1);
    });

    it('skips the canary when no dead_letter is configured', function () {
        bindFakeProbe($this->app, canaryError: null);
        doctorConnection('rabbitmq', ['exchange' => 'laravel.jobs']);

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->not->toContain('dead-letter canary');
    });

    it('skips the canary when the broker is unreachable', function () {
        bindFakeProbe($this->app, brokerError: 'connection refused');
        doctorDeadLetterConnection();

        Artisan::call('rabbit-rs:doctor');
        $output = Artisan::output();

        expect($output)->not->toContain('dead-letter canary');
    });
});
```

Extend `bindFakeProbe()` with an optional `?string $canaryError = null` constructor arg and an overridden `deadLetterCanary(...): ?string` returning it (the anonymous class already extends `DoctorProbe`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `php artisan test packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`
Expected: the four new cases FAIL (no canary check exists).

- [ ] **Step 3: Implement `DoctorProbe::deadLetterCanary()`**

In `DoctorProbe.php`, add after `declareTopology()`:

```php
/**
 * Dead-letter canary: publishes a uniquely marked probe into the connection's
 * main queue, consumes it, rejects it terminally, and asserts the broker
 * dead-letters it into the configured DLQ. This is a behavioral check with
 * real broker traffic — it exercises queue args → DLX → binding → DLQ, which
 * static topology checks cannot prove. Returns the error message, or null
 * when the canary was delivered.
 *
 * Foreign messages encountered in the main queue are released untouched
 * (never acked); on a DLQ backlog the verification loop requeues non-matching
 * messages and stays bounded. Run in quiet windows when possible.
 *
 * @param array<string, mixed> $nativeConfig
 */
public function deadLetterCanary(
    array $nativeConfig,
    string $broker,
    string $exchange,
    string $routingKey,
    string $dlq,
    string $workerProfile,
    string $managementUrl,
): ?string {
    return $this->probePool($nativeConfig, function (Pool $pool) use ($broker, $exchange, $routingKey, $dlq, $workerProfile, $managementUrl): void {
        $messageId = 'doctor-dlx-canary-'.bin2hex(random_bytes(8));

        $pool->publish([
            'broker' => $broker,
            'exchange' => $exchange,
            'routing_key' => $routingKey,
            'payload' => 'rabbit-rs doctor dead-letter canary',
            'message_id' => $messageId,
            'headers' => ['x-canary' => 'rabbit-rs-doctor'],
            'timeout_ms' => 5000,
        ]);

        $consumer = $pool->consumer($workerProfile);
        try {
            $rejected = false;
            $deadline = microtime(true) + 10.0;
            while (! $rejected && microtime(true) < $deadline) {
                $delivery = $consumer->next(2000);
                if ($delivery === null) {
                    continue;
                }
                if (($delivery->metadata()['message_id'] ?? '') === $messageId) {
                    $delivery->reject(requeue: false);
                    $rejected = true;
                } else {
                    $delivery->release(); // foreign traffic: never acked, never dropped
                }
            }
        } finally {
            $consumer->close();
        }

        if (! $rejected) {
            throw new RuntimeException('canary message was not consumed from the main queue within 10s');
        }

        $this->assertCanaryOnDlq($managementUrl, $dlq, $messageId);
    });
}
```

Add the DLQ verification helper (PHP side, matching `RabbitMqStatusCommand::fetchQueueStats()`'s management-API pattern):

```php
/**
 * Asserts the canary reached the DLQ via the management API. The probe is
 * pulled with ack_requeue_true and re-fetched with ack_requeue_false so only
 * the canary is removed; foreign dead-lettered messages are requeued
 * untouched. Bounded to 20 attempts (DLQ backlog).
 */
private function assertCanaryOnDlq(string $managementUrl, string $dlq, string $messageId): void
{
    $base = rtrim(trim($managementUrl), '/');
    for ($attempt = 0; $attempt < 20; $attempt++) {
        $body = json_encode(['count' => 1, 'ackmode' => 'ack_requeue_true', 'encoding' => 'auto', 'truncate' => 50_000], JSON_THROW_ON_ERROR);
        $http = curl_init("{$base}/api/queues/%2F/".rawurlencode($dlq).'/get');
        curl_setopt_array($http, [
            CURLOPT_POST => true,
            CURLOPT_POSTFIELDS => $body,
            CURLOPT_RETURNTRANSFER => true,
            CURLOPT_HTTPHEADER => ['Content-Type: application/json'],
            CURLOPT_HTTPAUTH => CURLAUTH_BASIC,
            CURLOPT_USERPWD => 'guest:guest',
        ]);
        $raw = curl_exec($http);
        curl_close($http);

        $messages = json_decode((string) $raw, true) ?: [];
        $first = $messages[0] ?? null;
        if ($first !== null && str_contains((string) ($first['payload'] ?? ''), $messageId)) {
            $this->removeFromDlq($base, $dlq); // ack_requeue_false pull removes only the canary

            return;
        }
        usleep(200_000);
    }

    throw new RuntimeException('canary message not found in DLQ within the verification window');
}

private function removeFromDlq(string $base, string $dlq): void
{
    $body = json_encode(['count' => 1, 'ackmode' => 'ack_requeue_false', 'encoding' => 'auto', 'truncate' => 50_000], JSON_THROW_ON_ERROR);
    $http = curl_init("{$base}/api/queues/%2F/".rawurlencode($dlq).'/get');
    curl_setopt_array($http, [
        CURLOPT_POST => true,
        CURLOPT_POSTFIELDS => $body,
        CURLOPT_RETURNTRANSFER => true,
        CURLOPT_HTTPHEADER => ['Content-Type: application/json'],
        CURLOPT_HTTPAUTH => CURLAUTH_BASIC,
        CURLOPT_USERPWD => 'guest:guest',
    ]);
    curl_exec($http);
    curl_close($http);
}
```

Notes:
- The probe pool publishes on the compiled publisher config; in safe mode the terminal reject → DLX return path is what proves the wiring.
- vhost `/` is hard-encoded as `%2F` here to match the existing management-API call sites; parameterize (`rawurlencode($vhost)`) if the doctor later passes it down.

- [ ] **Step 4: Wire the check into the doctor**

In `RabbitMqDoctorCommand.php`, in `doctorConnection()` after `checkTopology(...)`:

```php
$this->checkDeadLetterCanary($name, $compiled, $config, $probe, $brokerError);
```

And the method:

```php
/**
 * Behavioral dead-letter probe: publishes, terminally rejects, and asserts
 * DLQ delivery. Skipped without a reachable broker, without the extension,
 * or when no dead_letter topology is configured (checkTopology already
 * warns about that gap).
 *
 * @param  array<string, mixed>  $compiled
 * @param  array<string, mixed>  $config
 */
private function checkDeadLetterCanary(string $name, array $compiled, array $config, DoctorProbe $probe, ?string $brokerError): void
{
    $deadLetter = $compiled['topology']['dead_letter'] ?? null;
    if (! is_array($deadLetter) || $brokerError !== null) {
        return;
    }

    $broker = $compiled['native']['brokers'][0]['name'] ?? 'default';
    $route = $compiled['routes']['default'];
    $queue = $compiled['native']['workers'][0]['subscriptions'][0]['queue'] ?? null;
    $workerProfile = (string) ($compiled['native']['workers'][0]['name'] ?? $name);
    if ($queue === null) {
        return;
    }

    $error = $probe->deadLetterCanary(
        $compiled['native'],
        (string) $broker,
        (string) ($route['exchange'] ?? ''),
        str_replace('{queue}', (string) $queue, (string) ($route['routing_key'] ?? '{queue}')),
        (string) $deadLetter['queue'],
        $workerProfile,
        (string) ($config['management_url'] ?? ''),
    );

    if ($error !== null) {
        $this->emit('fail', "dead-letter canary failed: {$error} — dead-lettered messages would vanish (real broker traffic was produced)");

        return;
    }

    $this->emit('ok', 'dead-letter canary: delivered, rejected, and received on the DLQ');
}
```

- [ ] **Step 5: Run the unit tests to verify they pass**

Run: `php artisan test packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php`
Expected: new cases PASS, pre-existing suite green.

- [ ] **Step 6: Write the integration test (ext + lab broker)**

Create `packages/laravel-queue/tests/Integration/DoctorDlxCanaryTest.php`:

```php
<?php

declare(strict_types=1);

use Illuminate\Support\Facades\Artisan;

it('proves the configured dead-letter wiring end-to-end through the doctor canary', function () {
    config()->set('queue.connections.rabbit-rs-canary', [
        'driver' => 'rabbit-rs',
        'queue' => 'dlx-canary-main',
        'exchange' => 'dlx-canary-ex',
        'topology_mode' => 'declare',
        'queue_type' => 'classic',
        'dead_letter' => ['exchange' => 'dlx-canary-dlx', 'queue' => 'dlx-canary-dead'],
        'management_url' => env('RABBIT_RS_MANAGEMENT_URL', 'http://localhost:15672'),
        'username' => 'guest',
        'password' => 'guest',
    ]);

    Artisan::call('rabbit-rs:doctor', ['--connection' => ['rabbit-rs-canary']]);
    $output = Artisan::output();

    expect($output)->toContain('dead-letter canary: delivered')
        ->and(Artisan::call('rabbit-rs:doctor', ['--connection' => ['rabbit-rs-canary']]))->toBe(0);
});
```

- [ ] **Step 7: Run the integration suite**

Run: `./scripts/test-laravel.sh` (Integration group, requires the lab broker + ext built).
Expected: the new integration test PASSES; the canary queue and DLQ are left with no canary residue (probe removed from the DLQ by the verification helper).

- [ ] **Step 8: Commit**

```bash
git add packages/laravel-queue/src/Console/DoctorProbe.php packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php packages/laravel-queue/tests/Unit/Console/RabbitMqDoctorCommandTest.php packages/laravel-queue/tests/Integration/DoctorDlxCanaryTest.php
git commit -m "feat(laravel): dead-letter canary in rabbit-rs:doctor (#219)"
```

---

## Self-review notes

- Spec coverage: #252 (broker-side observability → Task 3), #219 (canary → Task 4), #254 (ceiling docs → Task 1), #255 (measurement → Task 2). Item 1 of #253 (quorum-TTL) is docs-only by decision; items 4/5 of #253 are fixed on main (no task).
- Parallel safety: Tasks 1, 2, 3 start in parallel; Task 4 starts after Task 3 merges (same file).
- Type consistency: `checkPublishOutcomes(array, array, bool)` matches its call site; `deadLetterCanary(...): ?string` matches both the probe method and the doctor's call; `bindFakeProbe(..., canaryError:)` extension is additive with existing defaults.
- Known caveat carried intentionally: the canary's DLQ verification hardcodes vhost `/` (%2F) like the existing management call sites; parameterization is deferred until the doctor passes vhost around.
