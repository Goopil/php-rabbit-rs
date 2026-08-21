#!/usr/bin/env php
<?php

declare(strict_types=1);

const BASE_DIR = __DIR__ . '/..';

spl_autoload_register(static function (string $class): void {
    $prefixes = [
        'Bench\\Drivers\\' => BASE_DIR . '/drivers/',
        'Bench\\Scenarios\\' => BASE_DIR . '/src/Scenarios/',
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

use Bench\Config;
use Bench\Drivers;

$scenarioFilter = null;
$driverFilter = null;
foreach (array_slice($argv, 1) as $arg) {
    if (str_starts_with($arg, '--scenario=')) {
        $scenarioFilter = substr($arg, strlen('--scenario='));
    }
    if (str_starts_with($arg, '--driver=')) {
        $driverFilter = substr($arg, strlen('--driver='));
    }
}

// Detect available drivers
$drivers = [];
if (extension_loaded('rabbit_rs')) {
    $drivers['rabbit-rs'] = Drivers\RabbitRsDriver::class;
}
if (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)) {
    $drivers['amqplib'] = Drivers\PhpAmqplibDriver::class;
}
if (extension_loaded('amqp')) {
    $drivers['amqp-ext'] = Drivers\AmqpExtDriver::class;
}

$scenarios = [
    'fire-and-forget' => \Bench\Scenarios\FireAndForgetBenchmark::class,
    'batch-confirm' => \Bench\Scenarios\BatchConfirmBenchmark::class,
    'auto-ack' => \Bench\Scenarios\AutoAckBenchmark::class,
];

$allResults = [];

foreach ($scenarios as $scenarioName => $scenarioClass) {
    if ($scenarioFilter !== null && $scenarioName !== $scenarioFilter) {
        continue;
    }

    foreach ($drivers as $driverName => $driverClass) {
        if ($driverFilter !== null && $driverName !== $driverFilter) {
            continue;
        }

        echo "\n=== {$scenarioName} / {$driverName} ===\n";

        try {
            $driver = new $driverClass();
            $benchmark = new $scenarioClass($driver);
            $benchmark->setUp();
            $stats = $benchmark->runBenchmark();
            $benchmark->tearDown();

            $allResults[$scenarioName . '/' . $driverName] = $stats;

            printf("  Publish: avg %.0f msg/s (min %.0f, max %.0f)\n",
                $stats['publish']['avg_rate'], $stats['publish']['min_rate'], $stats['publish']['max_rate']);
            printf("  Consume: avg %.0f msg/s (min %.0f, max %.0f)\n",
                $stats['consume']['avg_rate'], $stats['consume']['min_rate'], $stats['consume']['max_rate']);
            printf("  Latency p50: %.2f ms, p95: %.2f ms, p99: %.2f ms\n",
                $stats['consume']['p50'], $stats['consume']['p95'], $stats['publish']['p99']);
        } catch (\Throwable $e) {
            echo "  SKIP: {$e->getMessage()}\n";
        }
    }
}

// Write JSON results
$resultsFile = __DIR__ . '/../results/benchmark-results.json';
@mkdir(dirname($resultsFile), recursive: true);
file_put_contents($resultsFile, json_encode($allResults, JSON_PRETTY_PRINT));
echo "\nResults written to {$resultsFile}\n";
