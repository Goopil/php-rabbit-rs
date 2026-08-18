<?php

declare(strict_types=1);

use Bench\Drivers\AmqpExtDriver;
use Bench\Drivers\Driver;
use Bench\Drivers\PhpAmqplibDriver;
use Bench\Drivers\RabbitRsDriver;

const BASE_DIR = __DIR__;

spl_autoload_register(static function (string $class): void {
    $prefixes = [
        'Bench\\Drivers\\' => BASE_DIR . '/drivers/',
        'Bench\\' => BASE_DIR . '/lib/',
    ];
    foreach ($prefixes as $prefix => $base) {
        if (str_starts_with($class, $prefix)) {
            $relative = substr($class, strlen($prefix));
            $file = $base . str_replace('\\', '/', $relative) . '.php';
            if (is_file($file)) {
                require $file;
            }
            return;
        }
    }
});

if (is_file(BASE_DIR . '/vendor/autoload.php')) {
    require BASE_DIR . '/vendor/autoload.php';
}

function fail(string $message): never
{
    fwrite(STDERR, "ERROR: {$message}\n");
    exit(1);
}

function parseArgs(array $argv): array
{
    $opts = [
        'driver' => 'all',
        'safety' => 'all',
        'count' => 5000,
        'payload' => 1024,
        'batch' => 64,
        'output' => null,
    ];
    $count = count($argv);
    for ($i = 1; $i < $count; $i++) {
        $arg = $argv[$i];
        if (str_starts_with($arg, '--')) {
            $parts = explode('=', substr($arg, 2), 2);
            $key = $parts[0];
            $value = $parts[1] ?? ($argv[$i + 1] ?? '');
            if (!str_starts_with((string) $value, '--') && $value !== '') {
                if ($key === 'help') {
                    printUsage();
                    exit(0);
                }
                if (array_key_exists($key, $opts)) {
                    $opts[$key] = is_numeric($value) ? (int) $value : $value;
                    if (!str_contains($arg, '=')) {
                        $i++;
                    }
                }
                continue;
            }
            if ($key === 'help') {
                printUsage();
                exit(0);
            }
        }
    }
    return $opts;
}

function printUsage(): void
{
    fwrite(STDOUT, "Usage: php compare.php [options]\n\n");
    fwrite(STDOUT, "Options:\n");
    fwrite(STDOUT, "  --driver   rabbit-rs|php-amqplib|amqp-ext|all  (default: all)\n");
    fwrite(STDOUT, "  --safety   unsafe|confirms|safest|all           (default: all)\n");
    fwrite(STDOUT, "  --count    number of messages                   (default: 5000)\n");
    fwrite(STDOUT, "  --payload  payload size in bytes               (default: 1024)\n");
    fwrite(STDOUT, "  --batch    batch size                          (default: 64)\n");
    fwrite(STDOUT, "  --output   write JSON results to this path\n");
    fwrite(STDOUT, "  --help     show this help\n");
}

function formatNumber(float $n): string
{
    if ($n >= 1000) {
        return number_format($n, 0);
    }
    return number_format($n, 1);
}

function formatPad(string $s, int $width): string
{
    return str_pad($s, $width);
}

$opts = parseArgs($argv);

$driverNames = $opts['driver'] === 'all'
    ? ['rabbit-rs', 'php-amqplib', 'amqp-ext']
    : [$opts['driver']];

$safetyModes = $opts['safety'] === 'all'
    ? ['unsafe', 'confirms', 'safest']
    : [$opts['safety']];

$count = $opts['count'];
$payloadSize = $opts['payload'];
$batchSize = $opts['batch'];

fwrite(STDOUT, "Comparative Benchmark\n");
fwrite(STDOUT, "======================\n\n");

fwrite(STDOUT, "Environment:\n");
fwrite(STDOUT, "  PHP: " . PHP_VERSION . "\n");
fwrite(STDOUT, "  SAPI: " . PHP_SAPI . "\n");
fwrite(STDOUT, "  Broker: 127.0.0.1:5672\n");
fwrite(STDOUT, "  rabbit_rs: " . (extension_loaded('rabbit_rs') ? 'loaded' : 'not loaded') . "\n");
fwrite(STDOUT, "  amqp: " . (extension_loaded('amqp') ? 'loaded' : 'not loaded') . "\n");
fwrite(STDOUT, "  php-amqplib: " . (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class) ? 'available' : 'not available') . "\n\n");

fwrite(STDOUT, "Parameters: {$count} msgs, {$payloadSize}B payload, batch {$batchSize}\n\n");

$payload = str_repeat('x', $payloadSize);
$messages = [];
for ($i = 0; $i < $count; $i++) {
    $messages[] = $payload;
}

$results = [];

foreach ($driverNames as $driverName) {
    foreach ($safetyModes as $safety) {
        fwrite(STDOUT, "Running {$driverName} / {$safety}... ");

        $driver = null;
        try {
            $driver = match ($driverName) {
                'rabbit-rs' => extension_loaded('rabbit_rs') ? new RabbitRsDriver() : null,
                'php-amqplib' => class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)
                    ? new PhpAmqplibDriver()
                    : null,
                'amqp-ext' => extension_loaded('amqp') ? new AmqpExtDriver() : null,
                default => null,
            };
        } catch (\Throwable $e) {
            fwrite(STDOUT, "SKIP (" . $e->getMessage() . ")\n");
            continue;
        }

        if ($driver === null) {
            fwrite(STDOUT, "SKIP (not available)\n");
            continue;
        }

        try {
            $driver->setup();
            $driver->reset();

            $driver->resetLatencies();
            $driver->startTimer();

            $batches = array_chunk($messages, $batchSize);
            foreach ($batches as $batch) {
                $driver->publish($batch, $safety);
            }
            $publishElapsed = $driver->elapsedSeconds();
            $publishP99 = $driver->percentile(99);

            $driver->resetLatencies();
            $driver->startTimer();

            $driver->consume($count);
            $consumeElapsed = $driver->elapsedSeconds();
            $consumeP99 = $driver->percentile(99);

            $metrics = $driver->metrics();

            $publishThroughput = $publishElapsed > 0 ? $count / $publishElapsed : 0.0;
            $consumeThroughput = $consumeElapsed > 0 ? $count / $consumeElapsed : 0.0;

            $results[] = [
                'driver' => $driverName,
                'safety' => $safety,
                'publish_throughput' => $publishThroughput,
                'consume_throughput' => $consumeThroughput,
                'publish_p99_ms' => $publishP99,
                'consume_p99_ms' => $consumeP99,
                'rss_kb' => $metrics['rss_kb'],
                'cpu_seconds' => $metrics['cpu_seconds'],
                'losses' => $metrics['losses'],
            ];

            fwrite(STDOUT, "DONE\n");
        } catch (\Throwable $e) {
            fwrite(STDOUT, "FAIL (" . $e->getMessage() . ")\n");
        } finally {
            try {
                $driver?->teardown();
            } catch (\Throwable) {
            }
        }
    }
}

fwrite(STDOUT, "\n");

// --- Print comparison table ---
fwrite(STDOUT, "Results ({$count} msgs, {$payloadSize}B payload, batch {$batchSize}):\n\n");

$header = sprintf(
    "%-12s %-10s %16s %16s %14s %14s %10s %8s %7s\n",
    'Driver',
    'Safety',
    'Publish (msgs/s)',
    'Consume (msgs/s)',
    'p99 pub (ms)',
    'p99 cons (ms)',
    'RSS (KB)',
    'CPU (s)',
    'Losses',
);
fwrite(STDOUT, $header);
fwrite(STDOUT, str_repeat('-', strlen($header) - 1) . "\n");

foreach ($results as $r) {
    fwrite(STDOUT, sprintf(
        "%-12s %-10s %16s %16s %14s %14s %10s %8s %7d\n",
        $r['driver'],
        $r['safety'],
        formatNumber($r['publish_throughput']),
        formatNumber($r['consume_throughput']),
        formatNumber($r['publish_p99_ms']),
        formatNumber($r['consume_p99_ms']),
        number_format($r['rss_kb']),
        number_format($r['cpu_seconds'], 2),
        $r['losses'],
    ));
}
fwrite(STDOUT, "\n");

// --- Write results JSON ---
$resultsDir = BASE_DIR . '/results';
if (!is_dir($resultsDir)) {
    mkdir($resultsDir, 0755, true);
}

$timestamp = date('Ymd-His');
$defaultOutput = $resultsDir . '/compare-' . $timestamp . '.json';

$outputData = [
    'timestamp' => $timestamp,
    'environment' => [
        'php' => PHP_VERSION,
        'sapi' => PHP_SAPI,
        'broker' => '127.0.0.1:5672',
        'extensions' => [
            'rabbit_rs' => extension_loaded('rabbit_rs'),
            'amqp' => extension_loaded('amqp'),
            'php-amqplib' => class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class),
        ],
    ],
    'parameters' => [
        'count' => $count,
        'payload_bytes' => $payloadSize,
        'batch_size' => $batchSize,
    ],
    'results' => $results,
];

file_put_contents($defaultOutput, json_encode($outputData, JSON_PRETTY_PRINT));
fwrite(STDOUT, "Results written to: {$defaultOutput}\n");

if ($opts['output'] !== null) {
    file_put_contents($opts['output'], json_encode($outputData, JSON_PRETTY_PRINT));
    fwrite(STDOUT, "Results also written to: {$opts['output']}\n");
}

exit(0);
