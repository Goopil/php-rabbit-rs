#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
|--------------------------------------------------------------------------
| Phase E — driver-level queue benchmark runner
|--------------------------------------------------------------------------
|
| Bootstraps the minimal Laravel app, then drives the queue API of exactly
| one connection (one driver):
|
|   --mode=dispatch  Queue::push() x N, unit publishes (measured).
|   --mode=worker    Unmeasured mass fill (push x N), then measured
|                    pop + ack (delete) x N — same model as the Phase A
|                    laravel-worker scenario (fill blind, drain measured).
|
| Payload: the real Laravel queue envelope (JSON job payload) sized to
| ~1024 bytes, aligned with Config::MESSAGE_PAYLOAD_LARAVEL_BYTES (Phase A).
|
| Output: single JSON object on stdout (and optionally --output=PATH).
| Exit code 0 only when no message was lost (worker: received == count,
| queue empty afterwards; dispatch: every push resolved without error).
|
| Reliability/metric contract (Round J #127): every archived JSON surfaces
| per-op latency p50/p95/p99 (`latency_ms`), duplicates (worker: distinct
| job ids vs pops; dispatch: null — nothing is consumed), and the native
| pool's reconnects_total (rabbit-rs only; null elsewhere).
|
| Usage:
|   php bin/bench.php --connection=rabbit-rs --mode=dispatch --count=1000
|   php bin/bench.php --connection=rabbitmq-amqplib --mode=worker --count=1000 --rounds=3
*/

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Queue\QueueManager;
use Illuminate\Queue\Queue as BaseQueue;

require __DIR__.'/../vendor/autoload.php';

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

$args = [];
foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        $args[strtolower($m[1])] = $m[2];
    }
}

$connection = isset($args['connection']) ? (string) $args['connection'] : '';
$mode = strtolower((string) ($args['mode'] ?? 'dispatch'));
$count = max(1, (int) ($args['count'] ?? 10_000));
$rounds = max(1, (int) ($args['rounds'] ?? 1));
$outputPath = isset($args['output']) ? (string) $args['output'] : null;

if (! in_array($connection, ['rabbit-rs', 'rabbitmq-amqplib', 'rabbitmq-ext'], true)) {
    fwrite(STDERR, "error: --connection must be one of: rabbit-rs, rabbitmq-amqplib, rabbitmq-ext\n");
    exit(2);
}

if (! in_array($mode, ['dispatch', 'worker'], true)) {
    fwrite(STDERR, "error: --mode must be 'dispatch' or 'worker'\n");
    exit(2);
}

// ---------------------------------------------------------------------------
// Bootstrap the app (loads .env, config, registers + boots providers)
// ---------------------------------------------------------------------------

$app = require __DIR__.'/../bootstrap/app.php';

$consoleKernel = $app->make(Illuminate\Contracts\Console\Kernel::class);
$consoleKernel->bootstrap();

/** @var QueueManager $queueManager */
$queueManager = $app->make('queue');

$driverPackage = match ($connection) {
    'rabbit-rs' => 'goopil/rabbit-rs-laravel',
    'rabbitmq-amqplib' => 'vladimir-yuldashev/laravel-queue-rabbitmq',
    'rabbitmq-ext' => 'iamfarhad/laravel-rabbitmq',
};

$connectionConfig = (array) config("queue.connections.{$connection}", []);
$queueName = (string) ($connectionConfig['queue'] ?? 'bench.default');

$queue = $queueManager->connection($connection);

// ---------------------------------------------------------------------------
// Payload: real ~1024 B Laravel job envelope (aligned with Phase A)
// ---------------------------------------------------------------------------

const PAYLOAD_TARGET_BYTES = 1024;

$payloadData = buildPayloadData(0);
$bodySizeAtPadZero = measurePayloadBodySize($queue, $payloadData, $queueName);
$padBytes = max(0, PAYLOAD_TARGET_BYTES - $bodySizeAtPadZero);
$payloadData = buildPayloadData($padBytes);
$payloadBodyBytes = measurePayloadBodySize($queue, $payloadData, $queueName);
unset($bodySizeAtPadZero);

// ---------------------------------------------------------------------------
// Config echo (credentials masked)
// ---------------------------------------------------------------------------

$configEcho = maskCredentials($connectionConfig);
if ($connection === 'rabbit-rs') {
    // The goopil driver reads its broker/topology/publisher settings from
    // the global rabbit-rs config, not the connection entry: echo it too
    // (credentials masked) so the run records the effective broker config.
    $configEcho['rabbit_rs_global'] = maskCredentials((array) config('rabbit-rs', []));
}
$configEcho['connection'] = $connection;
$configEcho['mode'] = $mode;

// ---------------------------------------------------------------------------
// Warmup (unmeasured): purge leftovers from previous runs through the
// driver's own purge API. The driver may dispatch its warmup pop freely:
// a consumer that exists while the next fill is ingested receives every
// delivery (pre-fill delivery races fixed in the core, #37).
// ---------------------------------------------------------------------------

purgeQueue($queue, $connection, $queueName);

if ($mode === 'dispatch') {
    $queue->push('bench.noop', buildPayloadData($padBytes), $queueName);

    $drained = drainUntilEmpty($queue, $queueName, 1);
    if ($drained < 1) {
        fwrite(STDERR, "error: warmup message was not drained (got {$drained}/1) — aborting\n");
        exit(1);
    }
}

// ---------------------------------------------------------------------------
// Measured rounds
// ---------------------------------------------------------------------------

gc_collect_cycles();
gc_disable();

$roundsDetail = [];
$opsTotal = 0;
$timeTotal = 0.0;
$losses = 0;
$lateArrivals = 0;
$duplicates = null;
$opLatenciesMs = [];
$metrics = ['seen' => [], 'duplicates' => 0, 'op_latencies_ms' => []];

for ($round = 0; $round < $rounds; $round++) {
    if ($mode === 'worker') {
        if ($round > 0) {
            purgeQueue($queue, $connection, $queueName);
        }

        // Fill phase: mass dispatch, NOT measured (Phase A laravel-worker model).
        for ($i = 0; $i < $count; $i++) {
            $queue->push('bench.noop', $payloadData, $queueName);
        }
    }

    $started = hrtime(true);
    $received = 0;

    if ($mode === 'worker') {
        $received = drainUntilEmpty($queue, $queueName, $count, metrics: $metrics);
        $opLatenciesMs = [...$opLatenciesMs, ...$metrics['op_latencies_ms']];
        $duplicates = ($duplicates ?? 0) + $metrics['duplicates'];
    } else {
        for ($i = 0; $i < $count; $i++) {
            $opStart = hrtime(true);
            $queue->push('bench.noop', $payloadData, $queueName);
            $opLatenciesMs[] = (hrtime(true) - $opStart) / 1e6;
        }
        $received = $count;
    }

    $elapsed = (hrtime(true) - $started) / 1e9;

    if ($mode === 'worker' && $received < $count) {
        $losses += $count - $received;
    }

    $opsTotal += $received;
    $timeTotal += $elapsed;

    $roundsDetail[] = [
        'round' => $round,
        'ops' => $received,
        'time_s' => round($elapsed, 6),
        'rate_ops_s' => $elapsed > 0 ? round($received / $elapsed, 2) : null,
        // Stalls are no longer silently recovered: a null streak past the
        // plausible bound fails the run loudly, so this stays 0 by
        // construction.
        'stall_recoveries' => 0,
    ];
}

gc_enable();

// --- Post-run cleanup (dispatch): published rounds are never consumed, so
// without a final purge the queue keeps this run's backlog and the depth
// drifts across rounds and consecutive runs. Purge through the driver API
// (the pop-drain fallback is acceptable here: dispatch never consumes). ---
if ($mode === 'dispatch') {
    purgeQueue($queue, $connection, $queueName);
}

// ---------------------------------------------------------------------------
// Post-run verification: the queue must stay empty (worker mode). A settling
// window absorbs deliveries that are still in flight around the prefetch
// window when the measured drain completes; anything that surfaces there is
// acked and reported as a late arrival (a non-zero count fails the run).
// ---------------------------------------------------------------------------

if ($mode === 'worker') {
    $late = 0;
    $consecutiveNulls = 0;
    $settleDeadline = hrtime(true) + 5_000_000_000; // 5 s settling window

    while (hrtime(true) < $settleDeadline && $consecutiveNulls < 40) {
        $job = $queue->pop($queueName);

        if ($job === null) {
            $consecutiveNulls++;
            usleep(25_000);
            continue;
        }

        $consecutiveNulls = 0;
        $job->delete();
        $late++;
    }

    $lateArrivals = $late;
}

$rates = array_map(static fn (array $r) => $r['rate_ops_s'] ?? 0.0, $roundsDetail);
$rates = array_values(array_filter($rates, static fn ($r) => $r > 0.0));

// Native pool reconnects (rabbit-rs only): read from the same factory the
// connector uses, so the stats describe THIS run's pool. Other drivers do
// not surface a reconnect counter (null — not measured, never a zero claim).
$reconnects = null;
if ($connection === 'rabbit-rs') {
    try {
        $compiled = ConnectionCompiler::compile($connection, $connectionConfig, (array) config('rabbit-rs', []));
        $reconnects = $app->make(NativePoolFactory::class)
            ->make($compiled['native'])
            ->stats()['reconnects_total'] ?? null;
    } catch (\Throwable) {
        $reconnects = null;
    }
}

$ok = ($mode === 'dispatch' || ($losses === 0)) && $lateArrivals === 0;

$result = [
    'benchmark' => 'driver-bench',
    'phase' => 'E',
    'ok' => $ok,
    'connection' => $connection,
    'driver_package' => $driverPackage,
    'driver_package_version' => \Composer\InstalledVersions::getPrettyVersion($driverPackage),
    'mode' => $mode,
    'queue' => $queueName,
    'count' => $count,
    'rounds' => $rounds,
    'payload_body_bytes' => $payloadBodyBytes,
    'payload_target_bytes' => PAYLOAD_TARGET_BYTES,
    'ops_total' => $opsTotal,
    'time_total_s' => round($timeTotal, 6),
    'avg_rate_ops_s' => $timeTotal > 0 ? round($opsTotal / $timeTotal, 2) : null,
    'min_rate_ops_s' => $rates !== [] ? round(min($rates), 2) : null,
    'max_rate_ops_s' => $rates !== [] ? round(max($rates), 2) : null,
    'losses' => $losses,
    'duplicates' => $duplicates,
    'late_arrivals_after_drain' => $lateArrivals,
    'reconnects_total' => $reconnects,
    'latency_ms' => [
        'source' => $mode === 'dispatch' ? 'Queue::push call' : 'pop+ack (delete) call',
        'p50' => bench_percentile($opLatenciesMs, 0.50),
        'p95' => bench_percentile($opLatenciesMs, 0.95),
        'p99' => bench_percentile($opLatenciesMs, 0.99),
    ],
    'rounds_detail' => $roundsDetail,
    'config' => $configEcho,
    'meta' => [
        'php' => PHP_VERSION,
        'sapi' => PHP_SAPI,
        'extensions' => [
            'rabbit_rs' => phpversion('rabbit_rs') ?: false,
            'amqp' => phpversion('amqp') ?: false,
        ],
        'laravel' => \Composer\InstalledVersions::getPrettyVersion('laravel/framework'),
        'os' => PHP_OS.' '.php_uname('r'),
    ],
];

$json = json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
if ($json === false) {
    fwrite(STDERR, "error: failed to encode result JSON\n");
    exit(1);
}

fwrite(STDOUT, $json.PHP_EOL);

if ($outputPath !== null) {
    if (@file_put_contents($outputPath, $json.PHP_EOL) === false) {
        fwrite(STDERR, "error: failed to write output file: {$outputPath}\n");
        exit(1);
    }
}

exit($ok ? 0 : 1);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Job data array whose serialized envelope reaches PAYLOAD_TARGET_BYTES.
 * The 'payload' filler is a real byte body, never an empty string.
 */
function buildPayloadData(int $padBytes): array
{
    return [
        'index' => 0,
        'payload' => str_repeat('x', $padBytes),
        'meta' => ['origin' => 'driver-bench', 'phase' => 'E'],
    ];
}

/**
 * Measure the exact serialized job envelope length through the driver's
 * own payload creation path (same JSON every driver publishes).
 */
function measurePayloadBodySize(object $queue, array $data, string $queueName): int
{
    $measure = Closure::bind(
        static fn (object $q, string $job, string $qName, array $d): int => (int) strlen($q->createPayload($job, $qName, $d)),
        null,
        BaseQueue::class,
    );

    return $measure($queue, 'bench.noop', $queueName, $data);
}

/**
 * Purge leftovers from previous runs through the driver's own purge API
 * (method names differ per driver), falling back to a pop-drain.
 *
 * The fallback is best-effort: it never rebuilds the caller's connection.
 */
function purgeQueue(object $queue, string $connection, string $queueName): void
{
    $method = match ($connection) {
        'rabbit-rs' => 'clear',
        'rabbitmq-amqplib' => 'purge',
        'rabbitmq-ext' => 'purgeQueue',
    };

    if (method_exists($queue, $method)) {
        try {
            $queue->{$method}($queueName);

            return;
        } catch (Throwable) {
            // Fresh vhost: the queue may not exist yet — pop-drain instead.
        }
    }

    drainAll($queue, $queueName);
}

/**
 * Pop + ack (delete) until the queue stays empty for a while.
 *
 * @return int messages consumed
 */
function drainAll(object $queue, string $queueName): int
{
    // Purge fallback: the expected count is unknowable — drain until the
    // queue is observed empty for a short null streak (40 × 250 µs).
    // Best-effort by contract: never rebuilds the caller's connection and
    // never fails the run.
    $received = 0;
    $consecutiveNulls = 0;

    while ($consecutiveNulls < 40) {
        $job = $queue->pop($queueName);

        if ($job === null) {
            $consecutiveNulls++;
            usleep(250);
            continue;
        }

        $consecutiveNulls = 0;
        $job->delete();
        $received++;
    }

    return $received;
}

/**
 * Pop + ack (delete) until $expected messages are consumed or the queue is
 * observed empty too long to plausibly contain more work.
 *
 * Loud failure detection only: when a null streak runs past the plausible
 * bound while messages are still owed, the run FAILS with diagnostics
 * (driver, received, expected, streak length). Stalls are never silently
 * recovered here — a consumer that stops receiving while messages stay
 * ready is a core defect to root-cause, not a benchmark behavior to paper
 * over (Round I #126).
 *
 * When $metrics is provided (by-ref), the measured drain also records the
 * per-op pop+ack latency (ms) and duplicate deliveries via the job id
 * (at-least-once redeliveries are counted, never collapsed).
 *
 * @return int received (and acked) message count
 */
function drainUntilEmpty(
    object $queue,
    string $queueName,
    int $expected,
    ?int $nullCapOverride = null,
    ?array &$metrics = null,
): int {
    $received = 0;
    $consecutiveNulls = 0;
    $nullCap = $nullCapOverride ?? max(50_000, $expected * 50);
    $deadline = hrtime(true) + 120_000_000_000; // 120 s wall guard

    while ($received < $expected) {
        $opStart = hrtime(true);
        $job = $queue->pop($queueName);

        if ($job === null) {
            $consecutiveNulls++;

            if ($consecutiveNulls >= $nullCap) {
                fwrite(STDERR, sprintf(
                    "error: consumer stopped receiving while %d message(s) stay unaccounted for"
                        ." (received %d/%d, null streak %d, queue %s) — failing the run loudly instead of"
                        ." silently rebuilding the connection\n",
                    $expected - $received,
                    $received,
                    $expected,
                    $consecutiveNulls,
                    $queueName,
                ));
                exit(1);
            }

            if (hrtime(true) > $deadline) {
                break;
            }

            usleep(250);
            continue;
        }

        $consecutiveNulls = 0;
        $job->delete();
        $received++;

        if ($metrics !== null) {
            $metrics['op_latencies_ms'][] = (hrtime(true) - $opStart) / 1e6;

            $jobId = (string) $job->getJobId();
            if ($jobId !== '') {
                if (isset($metrics['seen'][$jobId])) {
                    $metrics['duplicates']++;
                } else {
                    $metrics['seen'][$jobId] = true;
                }
            }
        }
    }

    return $received;
}

/**
 * Latency percentile over per-op samples (ms); null when no ops were timed.
 */
function bench_percentile(array $latenciesMs, float $p): ?float
{
    if ($latenciesMs === []) {
        return null;
    }

    $sorted = $latenciesMs;
    sort($sorted);
    $index = (int) floor($p * count($sorted));

    return round($sorted[min($index, count($sorted) - 1)], 3);
}

/**
 * Config echo with credential-like values masked (never expose secrets).
 */
function maskCredentials(array $config): array
{
    $masked = [];
    foreach ($config as $key => $value) {
        if (is_string($key) && preg_match('/password|pass|secret|token|credential|user|username/i', $key) === 1) {
            $masked[$key] = '***';
            continue;
        }
        $masked[$key] = is_array($value) ? maskCredentials($value) : $value;
    }

    return $masked;
}
