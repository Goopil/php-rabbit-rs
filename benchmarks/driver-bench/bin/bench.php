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
| Usage:
|   php bin/bench.php --connection=rabbit-rs --mode=dispatch --count=1000
|   php bin/bench.php --connection=rabbitmq-amqplib --mode=worker --count=1000 --rounds=3
*/

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
$settleMs = max(0, (int) ($args['settle-ms'] ?? 500));
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
$queueName = (string) ($connectionConfig['queue'] ?? ($args['queue'] ?? 'bench.default'));

$queue = $queueManager->connection($connection);

/**
 * Rebuild the queue connection from scratch (fresh pools, channels and
 * consumers). For the ext-rabbit_rs driver the shared NativePoolFactory
 * must be flushed too: its pools are container-cached singletons and a
 * closed consumer set cannot be reopened on an existing pool.
 */
$reconnect = static function () use ($app, $queueManager, $connection): object {
    if ($connection === 'rabbit-rs') {
        $app->make(Goopil\RabbitRs\Laravel\Support\NativePoolFactory::class)->flush();
    }

    resetQueueConnection($queueManager, $connection);

    return $queueManager->connection($connection);
};

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
// driver's own purge API.
//
// IMPORTANT (worker mode): no pop may run before the measured drain. An
// ext-rabbit_rs consumer created before the fill and left idle while the
// fill is ingested misses deliveries (verified: consumer created pre-fill →
// ~2% of messages never surface; consumer created after the fill → clean).
// Worker mode therefore skips the pop warm-up entirely: the queue
// declaration happens with the first (unmeasured) fill push and the
// consumer is created by the first measured pop.
// ---------------------------------------------------------------------------

purgeQueue($queue, $connection, $queueName);

if ($mode === 'dispatch') {
    $queue->push('bench.noop', buildPayloadData($padBytes), $queueName);

    [$drained] = drainUntilEmpty($queue, $queueName, 1, null, $reconnect);
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

for ($round = 0; $round < $rounds; $round++) {
    if ($mode === 'worker') {
        if ($round > 0) {
            // Fresh connection per round: a consumer left over from the
            // previous round and left idle while the next fill is ingested
            // misses deliveries (see warmup note above).
            $queue = $reconnect();
            purgeQueue($queue, $connection, $queueName);
        }

        // Fill phase: mass dispatch, NOT measured (Phase A laravel-worker model).
        for ($i = 0; $i < $count; $i++) {
            $queue->push('bench.noop', $payloadData, $queueName);
        }

        // Settle (unmeasured): let the driver/broker delivery pipeline fully
        // ingest the fill before starting the timer. Without this, the tail
        // of the fill is still in flight when the first pops run and a few
        // messages surface late (observed on the ext-rabbit_rs consumer).
        usleep($settleMs * 1000);
    }

    $started = hrtime(true);
    $received = 0;

    if ($mode === 'worker') {
        [$received, $roundRecoveries] = drainUntilEmpty($queue, $queueName, $count, null, $reconnect);
    } else {
        for ($i = 0; $i < $count; $i++) {
            $queue->push('bench.noop', $payloadData, $queueName);
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
        'stall_recoveries' => $mode === 'worker' ? $roundRecoveries : 0,
    ];
}

gc_enable();

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
    'settle_ms' => $settleMs,
    'ops_total' => $opsTotal,
    'time_total_s' => round($timeTotal, 6),
    'avg_rate_ops_s' => $timeTotal > 0 ? round($opsTotal / $timeTotal, 2) : null,
    'min_rate_ops_s' => $rates !== [] ? round(min($rates), 2) : null,
    'max_rate_ops_s' => $rates !== [] ? round(max($rates), 2) : null,
    'losses' => $losses,
    'late_arrivals_after_drain' => $lateArrivals,
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
 * Purging via the driver API (no pops) matters for worker mode: any pop
 * before the measured drain creates the consumer early and breaks the
 * post-fill delivery pipeline on the ext-rabbit_rs driver.
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
 * Drop the cached queue connection so the next resolution rebuilds it from
 * scratch (fresh pools, channels and consumers). QueueManager has no public
 * forget API, so the protected connection cache is reset via reflection.
 */
function resetQueueConnection(object $queueManager, string $name): void
{
    $prop = new ReflectionProperty(get_class($queueManager), 'connections');
    $prop->setAccessible(true);

    $connections = $prop->getValue($queueManager);
    unset($connections[$name]);
    $prop->setValue($queueManager, $connections);
}

/**
 * Pop + ack (delete) until the queue stays empty for a while.
 *
 * @return int messages consumed
 */
function drainAll(object $queue, string $queueName): int
{
    return drainUntilEmpty($queue, $queueName, 0, 1000)[0];
}

/**
 * Pop + ack (delete) until $expected messages are consumed or the queue is
 * observed empty too long to plausibly contain more work.
 *
 * Stall recovery: when a long null streak is observed mid-drain, the queue
 * connection is rebuilt from scratch (fresh pool + consumer) and the drain
 * continues — mirroring what a real worker does on an idle timeout. Under
 * unit pop+ack churn the ext-rabbit_rs consumer can stop receiving
 * deliveries while messages remain ready in the queue; recovery makes the
 * drain 0-loss without hiding the cost (the stall wait stays in the timer).
 *
 * @return array{0: int, 1: int} received (and acked) message count, stall recoveries performed
 */
function drainUntilEmpty(
    object &$queue,
    string $queueName,
    int $expected,
    ?int $nullCapOverride = null,
    ?Closure $reconnect = null,
): array {
    $received = 0;
    $consecutiveNulls = 0;
    $recoveries = 0;
    $nullCap = $nullCapOverride ?? max(50_000, $expected * 50);
    $stallRecoveryAfter = 400; // consecutive nulls (~0.1 s at 250 µs sleep)
    $deadline = hrtime(true) + 120_000_000_000; // 120 s wall guard

    while ($received < $expected) {
        $job = $queue->pop($queueName);

        if ($job === null) {
            $consecutiveNulls++;

            if (hrtime(true) > $deadline) {
                break;
            }

            if (
                $expected > 0
                && $consecutiveNulls >= $stallRecoveryAfter
                && $reconnect !== null
                && $recoveries < 10
            ) {
                $consecutiveNulls = 0;
                $recoveries++;
                $queue = $reconnect();
                continue;
            }

            if ($consecutiveNulls >= $nullCap) {
                break;
            }

            usleep(250);
            continue;
        }

        $consecutiveNulls = 0;
        $job->delete();
        $received++;
    }

    return [$received, $recoveries];
}

/**
 * Config echo with credential-like values masked (never expose secrets).
 */
function maskCredentials(array $config): array
{
    $masked = [];
    foreach ($config as $key => $value) {
        if (is_string($key) && preg_match('/password|pass|secret|token|credential/i', $key) === 1) {
            $masked[$key] = '***';
            continue;
        }
        $masked[$key] = is_array($value) ? maskCredentials($value) : $value;
    }

    return $masked;
}
