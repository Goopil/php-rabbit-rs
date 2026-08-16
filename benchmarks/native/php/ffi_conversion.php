<?php

declare(strict_types=1);

use Goopil\RabbitRs\Pool;

ini_set('memory_limit', '1G');

function config(): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => '/',
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [],
        'topology_mode' => 'external',
    ];
}

function environment(): array
{
    return [
        'php_version' => PHP_VERSION,
        'sapi' => PHP_SAPI,
        'thread_safety' => (defined('PHP_ZTS') && PHP_ZTS) ? 'ZTS' : 'NTS',
        'os_family' => PHP_OS_FAMILY,
        'kernel' => php_uname('r'),
        'cpu' => php_uname('m'),
        'rabbitmq' => getenv('RABBIT_BENCH_BROKER_URI') ?: 'n/a (no broker)',
        'extension' => extension_loaded('rabbit_rs') ? 'loaded' : 'not loaded',
    ];
}

function make_message(int $payloadSize, int $headerCount, string $messageId = 'msg'): array
{
    $message = [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => str_repeat('x', $payloadSize),
        'message_id' => $messageId,
        'timeout_ms' => 100,
    ];

    if ($headerCount > 0) {
        $headers = [];
        for ($i = 0; $i < $headerCount; $i++) {
            $headers["x-header-{$i}"] = match ($i % 4) {
                0 => true,
                1 => $i,
                2 => (float) $i,
                default => "value-{$i}",
            };
        }
        $message['headers'] = $headers;
    }

    return $message;
}

function bench_single_publish(Pool $pool, int $payloadSize, int $headerCount): array
{
    $iterations = 200;
    $message = make_message($payloadSize, $headerCount);

    $start = hrtime(true);
    for ($i = 0; $i < $iterations; $i++) {
        $localMessage = $message;
        $localMessage['message_id'] = "msg-{$i}";
        try {
            $pool->publish($localMessage);
        } catch (Goopil\RabbitRs\Exception $e) {
            // Expected: the pool has no connection, but the conversion path is exercised.
        }
    }
    $elapsed = hrtime(true) - $start;

    return [
        'operation' => 'publish',
        'payload_size' => $payloadSize,
        'header_count' => $headerCount,
        'iterations' => $iterations,
        'total_ns' => $elapsed,
        'per_call_ns' => (int) ($elapsed / $iterations),
    ];
}

function bench_batch_publish(Pool $pool, int $batchSize, int $payloadSize, int $headerCount): array
{
    $iterations = 20;
    $totalBatchSize = $batchSize * $payloadSize;
    $iterations = $totalBatchSize > 50 * 1024 * 1024 ? 5 : 20;

    $messages = [];
    for ($i = 0; $i < $batchSize; $i++) {
        $messages[] = make_message($payloadSize, $headerCount, "msg-{$i}");
    }

    $start = hrtime(true);
    for ($i = 0; $i < $iterations; $i++) {
        try {
            $pool->publishBatch($messages);
        } catch (Goopil\RabbitRs\Exception $e) {
            // Expected: no broker connection, but the full batch conversion path is measured.
        }
    }
    $elapsed = hrtime(true) - $start;

    unset($messages);

    return [
        'operation' => 'publishBatch',
        'batch_size' => $batchSize,
        'payload_size' => $payloadSize,
        'header_count' => $headerCount,
        'iterations' => $iterations,
        'total_ns' => $elapsed,
        'per_call_ns' => (int) ($elapsed / $iterations),
        'per_message_ns' => (int) ($elapsed / ($iterations * $batchSize)),
    ];
}

function bench_config_validation(): array
{
    $iterations = 100;
    $cfg = config();

    $start = hrtime(true);
    for ($i = 0; $i < $iterations; $i++) {
        $pool = new Pool($cfg);
        $pool->close();
    }
    $elapsed = hrtime(true) - $start;

    return [
        'operation' => 'config_validation',
        'iterations' => $iterations,
        'total_ns' => $elapsed,
        'per_call_ns' => (int) ($elapsed / $iterations),
    ];
}

$payloadSizes = [
    256 => '256B',
    1024 => '1KiB',
    10 * 1024 => '10KiB',
    100 * 1024 => '100KiB',
    1024 * 1024 => '1MiB',
];

$headerCounts = [0, 8, 32, 128];
$batchSizes = [1, 16, 64, 256];

echo "rabbit_rs native FFI conversion benchmark\n";
echo "=========================================\n\n";

$env = environment();
echo "Environment:\n";
foreach ($env as $key => $value) {
    echo "  {$key}: {$value}\n";
}
echo "\n";

if (!extension_loaded('rabbit_rs')) {
    fwrite(STDERR, "Error: rabbit_rs extension is not loaded. Run ./scripts/install.sh first.\n");
    exit(1);
}

echo "Configuration validation benchmark:\n";
$configResult = bench_config_validation();
printf(
    "  config_validation: %d iterations, %d ns/call\n\n",
    $configResult['iterations'],
    $configResult['per_call_ns'],
);

echo "Single publish by payload size and header count:\n";
$cfg = config();
$pool = new Pool($cfg);

foreach ($payloadSizes as $size => $label) {
    foreach ($headerCounts as $headerCount) {
        $result = bench_single_publish($pool, $size, $headerCount);
        printf(
            "  publish payload=%-6s headers=%-3d: %d ns/call\n",
            $label,
            $headerCount,
            $result['per_call_ns'],
        );
    }
}
echo "\n";

echo "Batch publish by batch size and payload size (headers=0):\n";
foreach ($batchSizes as $batchSize) {
    foreach ($payloadSizes as $size => $label) {
        $result = bench_batch_publish($pool, $batchSize, $size, 0);
        printf(
            "  publishBatch batch=%-3d payload=%-6s: %d ns/call, %d ns/message\n",
            $batchSize,
            $label,
            $result['per_call_ns'],
            $result['per_message_ns'],
        );
    }
}
echo "\n";

$pool->close();

echo "Done.\n";
