#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
 * Round D profiling probe (experimental, not part of the bench protocol):
 * publish N messages through the same path as the driver-bench dispatch cell
 * (Queue::push → Pool::publish), then dump Pool::stats() percentiles.
 * confirmation_latency = broker confirm RTT as observed by the publisher actor.
 *
 * Usage: php probe-publish-path.php <safety> <count> [--stats]
 */

$benchRoot = dirname(__DIR__, 1).'/bench-driver-bench';
$benchRoot = '/Users/zacharyvolpi/dev/perso/rabbit-rs/.worktrees/task/41-round-d/benchmarks/driver-bench';

require $benchRoot.'/vendor/autoload.php';

$safety = $argv[1] ?? 'safe';
$count = max(1, (int) ($argv[2] ?? 1000));
$headerCount = max(0, (int) ($argv[3] ?? 0));
$mode = strtolower((string) ($argv[3] ?? 'dispatch'));

$_SERVER['argv'] = ['bench.php'];
$app = require $benchRoot.'/bootstrap/app.php';
$kernel = $app->make(Illuminate\Contracts\Console\Kernel::class);
$kernel->bootstrap();

config(['queue.connections.rabbit-rs.safety' => $safety]);

/** @var Illuminate\Queue\QueueManager $queueManager */
$queueManager = $app->make('queue');
$queue = $queueManager->connection('rabbit-rs');
$queueName = config('queue.connections.rabbit-rs.queue', 'bench.default');

// Real Laravel envelope ~1024 B (same as bench.php).
$payloadData = [
    'index' => 0,
    'payload' => str_repeat('x', 940),
    'meta' => ['origin' => 'probe', 'phase' => 'round-d'],
];
$payloadRaw = json_encode($payloadData, JSON_THROW_ON_ERROR);
$headers = [];
for ($h = 0; $h < $headerCount; $h++) {
    $headers["h$h"] = 'val';
}

$push = function () use ($queue, $payloadData, $payloadRaw, $queueName, $headers, $headerCount): void {
    if ($headerCount > 0) {
        $queue->pushRaw($payloadRaw, $queueName, ['headers' => $headers]);
        return;
    }
    $queue->push('bench.noop', $payloadData, $queueName);
};

// Purge leftovers through the driver purge API.
try {
    $queue->clear($queueName);
} catch (Throwable) {
    // fresh queue may not exist yet
}

gc_collect_cycles();
gc_disable();

// Warmup: one pool construction + first publish (connection setup) unmeasured.
$push();
$rp0 = new ReflectionProperty(get_class($queue), 'pool');
$rp0->setAccessible(true);
$rp0->getValue($queue)->flush();

$started = hrtime(true);
for ($i = 0; $i < $count; $i++) {
    $push();
}
$elapsed = (hrtime(true) - $started) / 1e9;

$rate = $count / $elapsed;

// Pull the pool out of the queue object to read stats + utilization.
$rp = new ReflectionProperty(get_class($queue), 'pool');
$rp->setAccessible(true);
$pool = $rp->getValue($queue);
$stats = $pool->stats();

printf(
    "safety=%s count=%d elapsed=%.3fs rate=%.0f msg/s\n".
    "confirm_latency_ms: p50=%d p95=%d p99=%d\n".
    "settlement_latency_ms: p50=%d p95=%d p99=%d\n".
    "publishes=%d confirmations=%d returns=%d backpressure=%d\n",
    $safety,
    $count,
    $elapsed,
    $rate,
    $stats['confirmation_latency_p50'],
    $stats['confirmation_latency_p95'],
    $stats['confirmation_latency_p99'],
    $stats['settlement_latency_p50'],
    $stats['settlement_latency_p95'],
    $stats['settlement_latency_p99'],
    $stats['publishes_total'],
    $stats['confirmations_total'],
    $stats['returns_total'],
    $stats['backpressure_total'],
);

exit(0);
