#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
 * Round L profile tool — Consumer::next() per-stage attribution.
 *
 * Runs tight native loops against the lab broker through the ext-rabbit_rs
 * extension only (no Laravel queue layer inside the measured section), one
 * stage per invocation:
 *
 *   null       next(0) on an empty queue        -> FFI + guards + drain + empty slow path
 *   trynull    tryNext() on an empty queue      -> FFI + guards + drain + empty fast path
 *   hot        next(1000) + ack() on a prefilled backlog -> + flume hand-off + delivery conversion
 *   hot_try    tryNext() + ack() on a prefilled backlog -> buffered fast path only
 *   batch      nextBatch(64, 1000) + ackBatch    -> amortized per-item cost
 *   earlyack   next(1000) on a no_ack subscription (early-ack spawn path)
 *
 * The driver-bench app is bootstrapped only to reuse its compiled native
 * configuration (ConnectionCompiler + NativePoolFactory): the measured
 * sections call the Pool/Consumer/Delivery classes directly.
 *
 * For profile runs, fill the queue with tools/fill.php first and pass
 * --skip-fill: the profiled process then records only the drain loop.
 *
 * Usage:
 *   php -d extension=<dylib> micro-consume.php --stage=hot --fill=30000
 *   php -d extension=<dylib> micro-consume.php --stage=hot --skip-fill=1 --iters=30000
 * Stages null/trynull ignore --fill.
 */

foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        $args[strtolower($m[1])] = $m[2];
    }
}

$stage = strtolower((string) ($args['stage'] ?? 'hot'));
$fill = max(1, (int) ($args['fill'] ?? 30_000));
$iters = (int) ($args['iters'] ?? 0);
$skipFill = (bool) ($args['skip-fill'] ?? false);
$poolDir = dirname(__DIR__, 3); // benchmarks/results/round-l-profile/tools -> benchmarks

require $poolDir.'/driver-bench/vendor/autoload.php';

$app = require $poolDir.'/driver-bench/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

/** @var Illuminate\Queue\QueueManager $queueManager */
$queueManager = $app->make('queue');
$connectionConfig = (array) config('queue.connections.rabbit-rs', []);
$queueName = (string) ($connectionConfig['queue'] ?? 'bench.goopil.driver-bench');

$compiled = Goopil\RabbitRs\Laravel\Config\ConnectionCompiler::compile(
    'rabbit-rs',
    $connectionConfig,
    (array) config('rabbit-rs', []),
);

if ($stage === 'earlyack') {
    // Flip the subscription onto the early-ack (no_ack) path so the actor
    // dispatches deliveries through the tokio::spawn ack branch.
    $compiled['native']['workers'][0]['subscriptions'][0]['early_ack'] = true;
    $compiled['native']['workers'][0]['subscriptions'][0]['no_ack'] = true;
}

$factory = $app->make(Goopil\RabbitRs\Laravel\Support\NativePoolFactory::class);
$pool = $factory->make($compiled['native']);

$broker = (string) $compiled['native']['brokers'][0]['name'];
$profile = (string) $compiled['native']['workers'][0]['name'];
$exchange = (string) ($compiled['native']['routes']['default']['exchange'] ?? '');
$routingKey = str_replace('{queue}', $queueName, (string) ($compiled['native']['routes']['default']['routing_key'] ?? $queueName));

// Purge leftovers from a previous run. Skipped with --skip-fill: the queue
// was prefilled by tools/fill.php in a separate process and must survive.
if (! $skipFill) {
    $pool->clear($broker, $queueName);
}

/**
 * @return list<array{broker: string, exchange: string, routing_key: string, payload: string, message_id: string, content_type: string}>
 */
function fillMessages(string $broker, string $routingKey, int $count, int $pad): array
{
    $messages = [];
    $payload = str_repeat('x', $pad);
    for ($i = 0; $i < $count; $i++) {
        $messages[] = [
            'broker' => $broker,
            'exchange' => '',
            'routing_key' => $routingKey,
            'payload' => $payload,
            'message_id' => 'round-l-'.$i.'-'.bin2hex(random_bytes(4)),
            'content_type' => 'application/json',
        ];
    }

    return $messages;
}

/** Waits until the queue holds $expected ready messages (bounded). */
function awaitIngestion(Goopil\RabbitRs\Pool $pool, string $broker, string $queue, int $expected): void
{
    $deadline = hrtime(true) + 30_000_000_000;
    while (hrtime(true) < $deadline) {
        if ($pool->size($broker, $queue) >= $expected) {
            return;
        }
        usleep(20_000);
    }
    fwrite(STDERR, sprintf("error: ingestion did not reach %d (at %d)\n", $expected, $pool->size($broker, $queue)));
    exit(2);
}

/**
 * Measures a loop; $op receives the iteration index and returns the measured
 * section. Records per-op latency in microseconds.
 *
 * @template T
 *
 * @param callable(int): T $op
 * @return array{p50: float, p95: float, p99: float, mean: float, max: float}
 */
function measure(int $iters, callable $op): array
{
    $latencies = [];
    for ($i = 0; $i < $iters; $i++) {
        $start = hrtime(true);
        $op($i);
        $latencies[] = (hrtime(true) - $start) / 1_000.0;
    }
    sort($latencies);
    $pick = static fn (float $q): float => $latencies[min(count($latencies) - 1, (int) floor($q * count($latencies)))];

    return [
        'p50' => round($pick(0.50), 3),
        'p95' => round($pick(0.95), 3),
        'p99' => round($pick(0.99), 3),
        'mean' => round(array_sum($latencies) / count($latencies), 3),
        'max' => round($latencies[count($latencies) - 1], 3),
    ];
}

// 1024 B payload, aligned with the driver-bench envelope.
$payloadPad = 1024 - 60; // envelope overhead is irrelevant here: the payload itself dominates

switch ($stage) {
    case 'null':
    case 'trynull':
        $consumer = $pool->consumer($profile);
        $empty = static fn () => null;
        $iters = $iters ?: 200_000;
        if ($stage === 'null') {
            $stats = measure($iters, static fn (): mixed => $consumer->next(0));
        } else {
            $stats = measure($iters, static fn (): mixed => $consumer->tryNext());
        }
        $consumed = 0;
        break;

    case 'hot':
    case 'hot_try':
    case 'earlyack':
        $iters = $iters ?: min($fill, 20_000);
        if (! $skipFill) {
            $messages = fillMessages($broker, $routingKey, $fill, $payloadPad);
            foreach (array_chunk($messages, 256) as $chunk) {
                $pool->publishBatch($chunk);
            }
            awaitIngestion($pool, $broker, $queueName, $fill);
        }
        // Consumer created after the fill (the pre-fill consumer misses
        // deliveries — documented driver-bench quirk 1).
        $consumer = $pool->consumer($profile);
        $consumed = 0;
        $nulls = 0;
        $op = match ($stage) {
            'hot' => static function () use ($consumer, &$consumed, &$nulls): mixed {
                $delivery = $consumer->next(1000);
                if ($delivery !== null) {
                    $delivery->ack();
                    $consumed++;
                    $nulls = 0;
                } else {
                    $nulls++;
                }

                return $delivery;
            },
            'hot_try' => static function () use ($consumer, &$consumed, &$nulls): mixed {
                $delivery = $consumer->tryNext();
                if ($delivery !== null) {
                    $delivery->ack();
                    $consumed++;
                    $nulls = 0;
                } else {
                    $nulls++;
                }

                return $delivery;
            },
            default => static function () use ($consumer, &$consumed, &$nulls): mixed {
                $delivery = $consumer->next(1000);
                if ($delivery !== null) {
                    $consumed++;
                    $nulls = 0;
                } else {
                    $nulls++;
                }

                return $delivery;
            },
        };
        // The loop stops at the null streak cap: once the backlog is drained
        // there is nothing left to measure, and next(1000) would otherwise
        // spend its full timeout per null pop.
        //
        // hot_try priming: tryNext() never blocks, so on a fresh consumer the
        // pump has not pushed into the flume buffer yet and 50 instant nulls
        // would end the loop before a single delivery arrives. Drain a few
        // deliveries through blocking next() first — that starts the pump and
        // leaves the buffer warm for the tryNext loop.
        if ($stage === 'hot_try') {
            for ($w = 0; $w < 64; $w++) {
                $warm = $consumer->next(1000);
                if ($warm !== null) {
                    $warm->ack();
                    $consumed++;
                }
            }
        }
        $latencies = [];
        for ($i = 0; $i < $iters && $nulls < 50; $i++) {
            $start = hrtime(true);
            $op();
            $latencies[] = (hrtime(true) - $start) / 1_000.0;
        }
        sort($latencies);
        $pick = static fn (float $q): float => $latencies[min(count($latencies) - 1, (int) floor($q * count($latencies)))];
        $stats = [
            'p50' => round($pick(0.50), 3),
            'p95' => round($pick(0.95), 3),
            'p99' => round($pick(0.99), 3),
            'mean' => round(array_sum($latencies) / count($latencies), 3),
            'max' => round($latencies[count($latencies) - 1], 3),
        ];
        break;

    case 'batch':
        $iters = $iters ?: min($fill, 20_000);
        if (! $skipFill) {
            $messages = fillMessages($broker, $routingKey, $fill, $payloadPad);
            foreach (array_chunk($messages, 256) as $chunk) {
                $pool->publishBatch($chunk);
            }
            awaitIngestion($pool, $broker, $queueName, $fill);
        }
        $consumer = $pool->consumer($profile);
        $consumed = 0;
        $batches = 0;
        $start = hrtime(true);
        while ($consumed < $iters) {
            $batchStart = hrtime(true);
            $batch = $consumer->nextBatch(64, 1000);
            $batchUs = (hrtime(true) - $batchStart) / 1_000.0;
            if ($batch === []) {
                fwrite(STDERR, "error: batch drain stalled with messages remaining\n");
                exit(2);
            }
            $consumer->ackBatch($batch);
            $consumed += count($batch);
            $batches++;
            $batchLatencies[] = $batchUs;
        }
        $elapsedS = (hrtime(true) - $start) / 1e9;
        sort($batchLatencies);
        $pick = static fn (float $q): float => $batchLatencies[min(count($batchLatencies) - 1, (int) floor($q * count($batchLatencies)))];
        $avgBatch = array_sum($batchLatencies) / count($batchLatencies);
        $avgSize = $consumed / $batches;
        $stats = [
            'per_item_us' => round($elapsedS / $consumed * 1e6, 3),
            'per_batch_us' => ['p50' => round($pick(0.5), 2), 'mean' => round($avgBatch, 2), 'p95' => round($pick(0.95), 2)],
            'avg_batch_size' => round($avgSize, 1),
            'drain_rate_items_s' => round($consumed / $elapsedS, 1),
        ];
        break;

    default:
        fwrite(STDERR, "error: unknown stage '{$stage}'\n");
        exit(2);
}

$consumer->close();
$pool->close();

$result = [
    'tool' => 'micro-consume',
    'stage' => $stage,
    'fill' => $fill,
    'requested_iters' => $iters,
    'consumed' => $consumed,
    'stats_us' => $stats,
    'meta' => [
        'php' => PHP_VERSION,
        'rabbit_rs' => phpversion('rabbit_rs') ?: false,
        'os' => PHP_OS.' '.php_uname('r'),
    ],
];

echo json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES), PHP_EOL;
