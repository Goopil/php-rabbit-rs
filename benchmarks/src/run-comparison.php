#!/usr/bin/env php
<?php

declare(strict_types=1);

/**
 * Issue #368 — public, reproducible driver comparison (rabbit-rs vs
 * php-amqplib vs ext-amqp) on the shared RabbitMQ lab.
 *
 * Three scenarios, same broker, same payload (1024 B), same QoS (128):
 *
 * - publish-fanout: 50,000 confirmed publications (batches of 256,
 *   persistent, mandatory where the driver supports it) into a durable
 *   queue; the publish rate is the metric, the drain is unmeasured.
 * - sustained-consume (the headline claim): blind batched fill (unmeasured),
 *   then a measured drain with push consumption and a manual ACK per
 *   message, prefetch 128 on every driver.
 * - broker-kill: same drain under load while the harness restarts the broker
 *   container; the run records survival, missing, duplicates, reconnects and
 *   harness recoveries. Drivers without built-in recovery get an identical
 *   harness-level restart between drain attempts (worker-restart semantics).
 *
 * Because ext-amqp only ships for PHP 8.4 locally while ext-rabbit_rs is
 * built for 8.5, each driver runs under the PHP binary that owns it
 * (run-comparison.sh orchestrates one invocation per driver and merges the
 * partials with --merge). All numbers land in one JSON envelope either way.
 */

const BASE_DIR = __DIR__ . '/..';

require_once BASE_DIR . '/vendor/autoload.php';

use Bench\AbstractBenchmark;
use Bench\Config;
use Bench\Drivers\AmqpExtDriver;
use Bench\Drivers\AmqplibDriver;
use Bench\Drivers\RabbitRsDriver;
use Bench\ResultMeta;
use Bench\ScenarioMode;

const THROUGHPUT_SCENARIOS = [
    // scenario => [mode, messages per round]
    'publish-fanout' => [ScenarioMode::BATCH_CONFIRM, 50000],
    'sustained-consume' => [ScenarioMode::SUSTAINED_CONSUME, 50000],
    // Same protocol as the archived round-l lineage (10,000 x N rounds,
    // 1024 B, prefetch 64) so the circulating headline claim is checkable
    // against the Laravel-worker shape it was made on.
    'laravel-worker' => [ScenarioMode::LARAVEL_WORKER, Config::MESSAGE_COUNT],
];

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

$opts = [
    'driver' => null,
    'scenario' => null,
    'output-dir' => null,
    'rounds' => 1,
    'warmup' => 1,
    'kill-count' => 100000,
    'kill-delay' => 2,
    'kill-timeout' => 240,
    'kill-container' => 'rabbitrs-rabbitmq-1-1',
    'merge' => false,
];
foreach (array_slice($argv, 1) as $arg) {
    if ($arg === '--merge') {
        $opts['merge'] = true;
        continue;
    }
    if (preg_match('/^--([a-z-]+)=(.*)$/', $arg, $m) !== 1 || !array_key_exists($m[1], $opts)) {
        fwrite(STDERR, "unknown option: {$arg}\n");
        exit(2);
    }
    $opts[$m[1]] = $m[1] === 'merge' ? filter_var($m[2], FILTER_VALIDATE_BOOL) : $m[2];
}
foreach (['rounds', 'warmup', 'kill-count', 'kill-delay', 'kill-timeout'] as $intOpt) {
    $opts[$intOpt] = (int) $opts[$intOpt];
}
$payloadBytes = Config::MESSAGE_PAYLOAD_LARAVEL_BYTES;
$outputDir = $opts['output-dir'] ?? BASE_DIR . '/results/comparison-run';
if (!is_dir($outputDir) && !mkdir($outputDir, recursive: true) && !is_dir($outputDir)) {
    fwrite(STDERR, "cannot create output dir: {$outputDir}\n");
    exit(2);
}

// ---------------------------------------------------------------------------
// Environment description (pinned next to the numbers, Round J contract)
// ---------------------------------------------------------------------------

function driver_versions(): array
{
    $versions = [
        'ext-rabbit_rs' => phpversion('rabbit_rs') ?: null,
        'ext-amqp' => phpversion('amqp') ?: null,
        'php-amqplib' => null,
    ];
    try {
        $versions['php-amqplib'] = \Composer\InstalledVersions::getPrettyVersion('php-amqplib/php-amqplib');
    } catch (\Throwable) {
    }
    return $versions;
}

function hardware_meta(): array
{
    $meta = ['os' => PHP_OS . ' ' . php_uname('r'), 'machine' => php_uname('m')];
    if (PHP_OS === 'Darwin') {
        foreach (['model' => 'hw.model', 'cpu' => 'machdep.cpu.brand_string', 'cores' => 'hw.ncpu', 'memory_bytes' => 'hw.memsize'] as $key => $sysctl) {
            $value = trim((string) shell_exec("sysctl -n {$sysctl} 2>/dev/null"));
            $meta[$key] = in_array($key, ['cores', 'memory_bytes'], true) ? (int) $value : $value;
        }
    }
    return $meta;
}

function broker_version(): ?string
{
    $context = stream_context_create(['http' => [
        'header' => 'Authorization: Basic ' . base64_encode(Config::RABBITMQ_USER . ':' . Config::RABBITMQ_PASSWORD),
        'timeout' => 3,
    ]]);
    $body = @file_get_contents('http://' . Config::RABBITMQ_HOST . ':15672/api/overview', false, $context);
    if ($body === false) {
        return null;
    }
    return json_decode($body, true)['rabbitmq_version'] ?? null;
}

/**
 * Per-driver configuration echo so the fairness claims are checkable against
 * the driver sources (benchmarks/src/Drivers/*.php).
 */
function fairness_echo(): array
{
    return [
        'payload_bytes' => Config::MESSAGE_PAYLOAD_LARAVEL_BYTES,
        'prefetch' => Config::PREFETCH_COUNT,
        'publish-fanout' => [
            'mode' => ScenarioMode::BATCH_CONFIRM,
            'publish' => 'confirmed, batches of 256, persistent; amqplib sets the mandatory flag, ext-amqp does not, rabbit-rs has no mandatory flag on this API',
            'consume (unmeasured drain)' => 'scenario consume path',
        ],
        'sustained-consume' => [
            'mode' => ScenarioMode::SUSTAINED_CONSUME,
            'fill (unmeasured)' => 'blind batched publish (256/batch), persistent',
            'drain (measured)' => 'push consumption (basic_consume / AMQPQueue::consume / dedicated consumer), manual ACK per message, prefetch 128',
        ],
        'laravel-worker' => [
            'mode' => ScenarioMode::LARAVEL_WORKER,
            'protocol' => '10,000 messages x rounds, 1024 B, prefetch 64 — same shape as the archived round-l lineage',
            'fill (unmeasured)' => 'blind publish (rabbit-rs: batched; amqplib/ext-amqp: per-message loop), persistent',
            'drain (measured)' => 'rabbit-rs: dedicated consumer pop + ACK per message; amqplib/ext-amqp: basic_get polling + ACK per message',
        ],
        'broker-kill' => [
            'mode' => ScenarioMode::SUSTAINED_CONSUME,
            'kill' => 'docker restart of the broker container (graceful SIGTERM shutdown + boot), armed KILL_DELAY seconds into the drain',
            'recovery' => 'driver-internal recovery if it has one; otherwise an identical harness-level driver restart (tearDown+setUp) between drain attempts until KILL_TIMEOUT',
        ],
        'rabbit-rs' => 'Pool: dedicated consumer channel, prefetch 128, early_ack=false, no_ack=false, heartbeat 30; publisher blind (sustained-consume) or safe confirms (publish-fanout), flush_interval 1 ms, persistent delivery enforced by the core',
        'amqplib' => 'php-amqplib 3.x AMQPStreamConnection: two connections (pub/cons), basic_qos(0,128), push consume via basic_consume, manual ACK per message; confirmed publish batches of 256 via wait_for_pending_acks(5), persistent, mandatory=true; heartbeat 60, read/write timeout 60/10',
        'amqp-ext' => 'pecl amqp: one connection, two channels, setPrefetchCount(128), push consume via AMQPQueue::consume, manual ACK per message; confirmed publish batches of 256 via waitForConfirm(5), persistent, mandatory not set; read timeout 1 s during consume',
    ];
}

// ---------------------------------------------------------------------------
// Driver detection (the three compared drivers only)
// ---------------------------------------------------------------------------

$driverClasses = [];
if (extension_loaded('rabbit_rs')) {
    $driverClasses['rabbit-rs'] = RabbitRsDriver::class;
}
if (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)) {
    $driverClasses['amqplib'] = AmqplibDriver::class;
}
if (extension_loaded('amqp')) {
    $driverClasses['amqp-ext'] = AmqpExtDriver::class;
}
if ($opts['driver'] !== null) {
    $driverClasses = array_intersect_key($driverClasses, [$opts['driver'] => true]);
    if ($driverClasses === []) {
        fwrite(STDERR, "driver not available under this PHP binary: {$opts['driver']}\n");
        exit(2);
    }
}

// ---------------------------------------------------------------------------
// Broker readiness (same gate as run-benchmarks.php)
// ---------------------------------------------------------------------------

$brokerReady = false;
for ($i = 0; $i < 30; $i++) {
    try {
        $connection = new \PhpAmqpLib\Connection\AMQPStreamConnection(
            Config::RABBITMQ_HOST,
            Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER,
            Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST,
        );
        $connection->close();
        $brokerReady = true;
        break;
    } catch (\Throwable) {
        sleep(1);
    }
}
if (!$brokerReady) {
    echo "Broker not ready after 30s\n";
    exit(1);
}

/**
 * Instantiate a driver configured for one comparison scenario.
 */
function make_driver(string $class, string $mode, int $payloadBytes): AbstractBenchmark
{
    $driver = new $class();
    $driver->setScenarioMode($mode);
    $driver->setPayloadBytes($payloadBytes);
    return $driver;
}

function run_throughput_round(AbstractBenchmark $driver, string $scenario, string $mode, int $count, int $round, bool $warmup): array
{
    $row = ['scenario' => $scenario, 'driver' => $driver->getName(), 'round' => $round, 'warmup' => $warmup, 'count' => $count, 'safety' => $driver->safetyMode()];    try {
        $driver->setUp();
        $driver->beginPhase();
        $driver->expectCount($count);
        $driver->purgeQueue();

        $start = hrtime(true);
        $driver->publishMessages($count);
        $publishSeconds = (hrtime(true) - $start) / 1e9;

        usleep(500_000); // settle: let buffered publications land before the drain

        $start = hrtime(true);
        $driver->consumeMessages($count);
        $consumeSeconds = (hrtime(true) - $start) / 1e9;

        $report = $driver->phaseReport();
        $row += [
            'publish_seconds' => round($publishSeconds, 3),
            'consume_seconds' => round($consumeSeconds, 3),
            'publish_rate' => round($count / $publishSeconds, 1),
            'consume_rate' => round($count / $consumeSeconds, 1),
            'losses' => $report['losses'],
            'duplicates' => $report['duplicates'],
            'received' => $report['received'],
            'latency_ms' => $report['latency_ms'],
            'reconnects' => $driver->reconnects(),
            'ok' => true,
        ];
    } catch (\Throwable $e) {
        $row += ['ok' => false, 'error' => $e->getMessage()];
    } finally {
        try {
            $driver->tearDown();
        } catch (\Throwable) {
            // Best-effort teardown.
        }
    }
    return $row;
}

function run_broker_kill(AbstractBenchmark $driver, int $count, int $killDelay, int $killTimeout, string $container): array
{
    $row = ['scenario' => 'broker-kill', 'driver' => $driver->getName(), 'round' => 0, 'warmup' => false, 'count' => $count, 'safety' => $driver->safetyMode()];
    try {
        $driver->setUp();
        $driver->beginPhase();
        $driver->expectCount($count);
        $driver->purgeQueue();

        $start = hrtime(true);
        $driver->publishMessages($count);
        $fillSeconds = (hrtime(true) - $start) / 1e9;
        usleep(500_000);

        // Arm the kill: a full container restart lands KILL_DELAY seconds into
        // the drain, while the consumer is under load.
        shell_exec(sprintf('(sleep %d; docker restart %s) >/dev/null 2>&1 &', $killDelay, escapeshellarg($container)));
        $row['kill'] = ['container' => $container, 'delay_seconds' => $killDelay, 'method' => 'docker restart (graceful SIGTERM + boot)'];

        $deadline = hrtime(true) / 1e9 + $killTimeout;
        $start = hrtime(true);
        $attempts = 0;
        $recoveries = 0;
        $reconnects = null;
        $lastError = null;
        $survived = false;
        while (true) {
            $attempts++;
            try {
                $driver->consumeMessages($count);
            } catch (\Throwable $e) {
                $lastError = $e->getMessage();
            }
            $current = $driver->reconnects();
            if ($current !== null) {
                // Max across pools: harness restarts swap the pool, so this
                // under-counts when a restart happened; noted in the README.
                $reconnects = max($reconnects ?? 0, $current);
            }
            if ($driver->phaseReport()['received'] >= $count) {
                $survived = true;
                break;
            }
            if (hrtime(true) / 1e9 > $deadline) {
                break;
            }
            // Identical harness-level recovery for every driver: restart the
            // driver (worker-process restart semantics), then retry the drain.
            try {
                $driver->tearDown();
            } catch (\Throwable) {
            }
            try {
                $driver->setUp();
            } catch (\Throwable $e) {
                $lastError = $e->getMessage();
            }
            $recoveries++;
            sleep(1);
        }
        $drainSeconds = (hrtime(true) - $start) / 1e9;
        $report = $driver->phaseReport();
        $row += [
            'fill_seconds' => round($fillSeconds, 3),
            'drain_seconds' => round($drainSeconds, 3),
            'attempts' => $attempts,
            'harness_recoveries' => $recoveries,
            'survived' => $survived,
            'received' => $report['received'],
            'losses' => $report['losses'],
            'duplicates' => $report['duplicates'],
            'latency_ms' => $report['latency_ms'],
            'reconnects' => $reconnects,
            'last_error' => $lastError,
            'ok' => true,
        ];
    } catch (\Throwable $e) {
        $row += ['ok' => false, 'error' => $e->getMessage()];
    } finally {
        try {
            $driver->tearDown();
        } catch (\Throwable) {
            // Best-effort teardown.
        }
    }
    return $row;
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

$partialsPath = $outputDir . '/partial-' . ($opts['driver'] ?? array_key_first($driverClasses)) . '.json';
$partial = is_file($partialsPath) ? (json_decode((string) file_get_contents($partialsPath), true) ?: []) : [];
$partial['driver_versions'] = array_filter(driver_versions());
$partial['php'] = PHP_VERSION;
$partial['runs'] = $partial['runs'] ?? [];

$env = [
    'issue' => 368,
    'date' => date('c'),
    'broker' => [
        'host' => Config::RABBITMQ_HOST,
        'port' => Config::RABBITMQ_PORT,
        'vhost' => Config::RABBITMQ_VHOST,
        'user' => '***',
        'version' => broker_version(),
        'lab_profile' => 'with-plugin',
        'kill_container' => $opts['kill-container'],
    ],
    'payload_bytes' => $payloadBytes,
    'counts' => [
        'publish-fanout' => 50000,
        'sustained-consume' => 50000,
        'laravel-worker' => Config::MESSAGE_COUNT,
        'broker-kill' => $opts['kill-count'],
    ],
    'hardware' => hardware_meta(),
    'fairness' => fairness_echo(),
];

if ($opts['merge']) {
    $merged = ['env' => $env, 'runs' => []];
    foreach (glob($outputDir . '/partial-*.json') ?: [] as $file) {
        $data = json_decode((string) file_get_contents($file), true);
        if (is_array($data) && isset($data['runs'])) {
            $merged['runs'] = array_merge($merged['runs'], $data['runs']);
        }
    }
    usort($merged['runs'], fn(array $a, array $b) => [$a['scenario'], $a['driver'], $a['round']] <=> [$b['scenario'], $b['driver'], $b['round']]);
    $merged['meta'] = ResultMeta::meta();
    $resultsFile = $outputDir . '/comparison-results.json';
    file_put_contents($resultsFile, json_encode($merged, JSON_PRETTY_PRINT));
    echo "Merged {$resultsFile}\n";
    summarize_and_exit($merged);
    exit;
}

$scenarioFilter = $opts['scenario'];
$rows = [];

foreach (THROUGHPUT_SCENARIOS as $scenario => [$mode, $scenarioCount]) {
    if ($scenarioFilter !== null && $scenarioFilter !== $scenario) {
        continue;
    }
    foreach (range(0, $opts['warmup'] + $opts['rounds'] - 1) as $round) {
        $warmup = $round < $opts['warmup'];
        foreach ($driverClasses as $name => $class) {
            echo "\n=== {$scenario} / {$name} / round {$round}" . ($warmup ? ' (warmup)' : '') . " ===\n";
            $row = run_throughput_round(make_driver($class, $mode, $payloadBytes), $scenario, $mode, $scenarioCount, $round, $warmup);
            printf("  Publish: %.0f msg/s | Consume: %.0f msg/s | losses %s | duplicates %s\n",
                $row['publish_rate'] ?? 0, $row['consume_rate'] ?? 0,
                isset($row['losses']) ? (string) $row['losses'] : 'n/a', isset($row['duplicates']) ? (string) $row['duplicates'] : 'n/a');
            $rows[] = $row;
        }
    }
}

if ($scenarioFilter === null || $scenarioFilter === 'broker-kill') {
    foreach ($driverClasses as $name => $class) {
        echo "\n=== broker-kill / {$name} ===\n";
        $driver = make_driver($class, ScenarioMode::SUSTAINED_CONSUME, $payloadBytes);
        $row = run_broker_kill($driver, $opts['kill-count'], $opts['kill-delay'], $opts['kill-timeout'], $opts['kill-container']);
        printf("  Survived: %s | received %d/%d | losses %s | duplicates %s | harness recoveries %d | drain %.1fs\n",
            $row['survived'] ?? 'false', $row['received'] ?? 0, $opts['kill-count'],
            isset($row['losses']) ? (string) $row['losses'] : 'n/a',
            isset($row['duplicates']) ? (string) $row['duplicates'] : 'n/a',
            $row['harness_recoveries'] ?? 0, $row['drain_seconds'] ?? 0);
        $rows[] = $row;
    }
}

$partial['runs'] = array_merge($partial['runs'], $rows);
file_put_contents($partialsPath, json_encode(['env' => $env, 'php' => PHP_VERSION, 'runs' => $partial['runs']], JSON_PRETTY_PRINT));
echo "\nPartial written to {$partialsPath}\n";

/**
 * Print the medians per scenario/driver and exit non-zero when an integrity
 * gate fails (house rule: losses/duplicates invalidate a run).
 */
function summarize_and_exit(array $merged): void
{
    $measured = array_filter($merged['runs'], fn(array $r) => empty($r['warmup']) && !empty($r['ok']));
    $groups = [];
    foreach ($measured as $r) {
        $groups[$r['scenario']][$r['driver']][] = $r;
    }
    $failed = false;
    printf("%-18s | %-10s | %-16s | %-16s | %-8s | %-8s\n", 'Scenario', 'Driver', 'Publish msg/s', 'Consume msg/s', 'Losses', 'Duplicates');
    echo str_repeat('-', 92) . "\n";
    foreach ($groups as $scenario => $perDriver) {
        foreach ($perDriver as $driver => $rows) {
            $median = fn(string $key): float => count($rows) ? median(array_column($rows, $key)) : 0.0;
            $losses = array_sum(array_column($rows, 'losses'));
            $duplicates = array_sum(array_column($rows, 'duplicates'));
            printf("%-18s | %-10s | %-16.0f | %-16.0f | %-8s | %-8s\n",
                $scenario, $driver, $median('publish_rate'), $median('consume_rate'), $losses, $duplicates);
            // broker-kill duplicates are expected (at-least-once redelivery
            // after the restart); losses and non-survival fail the gate.
            if ($scenario === 'broker-kill') {
                $survived = array_sum(array_map(fn(array $r) => (int) ($r['survived'] ?? false), $rows));
                if ($survived < count($rows) || $losses > 0) {
                    $failed = true;
                }
            } elseif ($losses > 0 || $duplicates > 0) {
                $failed = true;
            }
        }
    }
    foreach ($merged['runs'] as $r) {
        if (empty($r['ok'])) {
            echo "FAIL: {$r['scenario']}/{$r['driver']}: {$r['error']}\n";
            $failed = true;
        }
    }
    exit($failed ? 1 : 0);
}

function median(array $values): float
{
    if ($values === []) {
        return 0.0;
    }
    sort($values);
    $n = count($values);
    return $n % 2 === 1 ? $values[intdiv($n, 2)] : ($values[$n / 2 - 1] + $values[$n / 2]) / 2;
}
