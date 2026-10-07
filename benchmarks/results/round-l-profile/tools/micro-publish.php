<?php

declare(strict_types=1);

/*
 * Round L profile tool — publish path attribution at the extension boundary.
 *
 * Publishes N messages through Pool::publish / Pool::publishBatch with the
 * configured safety mode (RABBIT_RS_SAFETY env: safe|unsafe|blind, default
 * safe), measuring per-op latency inside the loop. The driver-bench app is
 * bootstrapped only to reuse its compiled native configuration; the measured
 * section contains no framework code.
 *
 * Usage:
 *   RABBIT_RS_SAFETY=safe php -d extension=<dylib> micro-publish.php --iters=20000 --mode=single
 *   RABBIT_RS_SAFETY=blind php -d extension=<dylib> micro-publish.php --iters=50000 --mode=single
 */

$args = [];
foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        $args[strtolower($m[1])] = $m[2];
    }
}

$mode = strtolower((string) ($args['mode'] ?? 'single'));
$iters = max(1, (int) ($args['iters'] ?? 20_000));
$props = strtolower((string) ($args['props'] ?? 'full'));
$poolDir = dirname(__DIR__, 3);

if (! in_array($mode, ['single', 'batch'], true)) {
    fwrite(STDERR, "error: --mode must be 'single' or 'batch'\n");
    exit(2);
}
if (! in_array($props, ['full', 'minimal', 'hdr1'], true)) {
    fwrite(STDERR, "error: --props must be 'full', 'minimal' or 'hdr1'\n");
    exit(2);
}

require $poolDir.'/driver-bench/vendor/autoload.php';

$app = require $poolDir.'/driver-bench/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

$connectionConfig = (array) config('queue.connections.rabbit-rs', []);
$queueName = (string) ($connectionConfig['queue'] ?? 'bench.goopil.driver-bench');

$compiled = Goopil\RabbitRs\Laravel\Config\ConnectionCompiler::compile(
    'rabbit-rs',
    $connectionConfig,
    (array) config('rabbit-rs', []),
);

$factory = $app->make(Goopil\RabbitRs\Laravel\Support\NativePoolFactory::class);
$pool = $factory->make($compiled['native']);

$broker = (string) $compiled['native']['brokers'][0]['name'];
$payload = str_repeat('x', 1024);
$latencies = [];

/**
 * Builds one publication envelope. `props` varies the per-publish string
 * properties so the decision gate can A/B the conversion cost:
 *   full    — message_id + content_type (the driver-bench envelope)
 *   minimal — message_id only (no content_type string on the wire path)
 *   hdr1    — message_id + content_type + one custom header
 *            (exercises the per-header budget-location formatting)
 */
function message(string $broker, string $queue, string $props, string $id, string $payload): array
{
    $message = [
        'broker' => $broker,
        'exchange' => '',
        'routing_key' => $queue,
        'payload' => $payload,
        'message_id' => $id,
    ];
    if ($props !== 'minimal') {
        $message['content_type'] = 'application/json';
    }
    if ($props === 'hdr1') {
        $message['headers'] = ['x-round-l-probe' => '1'];
    }

    return $message;
}

if ($mode === 'single') {
    for ($i = 0; $i < $iters; $i++) {
        $message = message($broker, $queueName, $props, 'round-l-pub-'.$i.'-'.bin2hex(random_bytes(4)), $payload);
        $start = hrtime(true);
        $pool->publish($message);
        $latencies[] = (hrtime(true) - $start) / 1_000.0;
    }
} else {
    $chunks = (int) ceil($iters / 256);
    for ($c = 0; $c < $chunks; $c++) {
        $messages = [];
        for ($i = 0; $i < 256; $i++) {
            $messages[] = message($broker, $queueName, $props, 'round-l-pub-'.$c.'-'.$i.'-'.bin2hex(random_bytes(4)), $payload);
        }
        $start = hrtime(true);
        $pool->publishBatch($messages);
        $latencies[] = (hrtime(true) - $start) / 1_000.0;
    }
}

// Flush any buffered publications (blind/unsafe) so stats() reports a settled state.
$pool->flush();
$stats = $pool->stats();
$pool->clear($broker, $queueName);
$pool->close();

sort($latencies);
$pick = static fn (float $q): float => $latencies[min(count($latencies) - 1, (int) floor($q * count($latencies)))];

$unit = $mode === 'single' ? 1 : 256; // batch: per-call latency covers 256 messages
$ops = count($latencies) * $unit;
$elapsedS = array_sum($latencies) / 1e6;

$result = [
    'tool' => 'micro-publish',
    'mode' => $mode,
    'props' => $props,
    'safety' => $compiled['native']['publisher']['safety'] ?? 'unknown',
    'iters' => $iters,
    'ops' => $ops,
    'rate_ops_s' => round($ops / $elapsedS, 1),
    'per_op_us' => $mode === 'single' ? [
        'p50' => round($pick(0.50), 3),
        'p95' => round($pick(0.95), 3),
        'p99' => round($pick(0.99), 3),
        'mean' => round(array_sum($latencies) / count($latencies), 3),
        'max' => round($latencies[count($latencies) - 1], 3),
    ] : [
        'p50' => round($pick(0.50), 2),
        'p95' => round($pick(0.95), 2),
        'p99' => round($pick(0.99), 2),
        'mean' => round(array_sum($latencies) / count($latencies), 2),
        'max' => round($latencies[count($latencies) - 1], 2),
    ],
    'native_stats' => [
        'publishes_total' => $stats['publishes_total'] ?? null,
        'confirmations_total' => $stats['confirmations_total'] ?? null,
        'returns_total' => $stats['returns_total'] ?? null,
        'reconnects_total' => $stats['reconnects_total'] ?? null,
        'publish_buffered' => $stats['publish_buffered'] ?? null,
        'dropped_publications_total' => $stats['dropped_publications_total'] ?? null,
        'confirmation_latency_us_p50' => $stats['confirmation_latency_p50'] ?? null,
    ],
    'meta' => [
        'php' => PHP_VERSION,
        'rabbit_rs' => phpversion('rabbit_rs') ?: false,
        'os' => PHP_OS.' '.php_uname('r'),
    ],
];

echo json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES), PHP_EOL;
