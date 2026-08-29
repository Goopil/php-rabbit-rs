#!/usr/bin/env php
<?php

declare(strict_types=1);

const BASE_DIR = __DIR__ . '/..';

spl_autoload_register(static function (string $class): void {
    $prefixes = [
        'Bench\\Drivers\\' => BASE_DIR . '/src/Drivers/',
        'Bench\\Scenarios\\' => BASE_DIR . '/src/Scenarios/',
        'Bench\\' => BASE_DIR . '/src/',
    ];
    foreach ($prefixes as $prefix => $base) {
        if (str_starts_with($class, $prefix)) {
            $relative = substr($class, strlen($prefix));
            $file = $base . str_replace('\\', '/', $relative) . '.php';
            if (is_file($file)) {
                require_once $file;
            }
            return;
        }
    }
});

if (is_file(BASE_DIR . '/vendor/autoload.php')) {
    require_once BASE_DIR . '/vendor/autoload.php';
}

use Bench\Budget;
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
    $drivers['amqplib'] = Drivers\AmqplibDriver::class;
}
if (extension_loaded('amqp')) {
    $drivers['amqp-ext'] = Drivers\AmqpExtDriver::class;
}
if (class_exists(\Bunny\Client::class)) {
    $drivers['bunny'] = Drivers\BunnyDriver::class;
}

$scenarios = [
    'fire-and-forget' => \Bench\Scenarios\FireAndForgetBenchmark::class,
    'batch-confirm' => \Bench\Scenarios\BatchConfirmBenchmark::class,
    'auto-ack' => \Bench\Scenarios\AutoAckBenchmark::class,
    'laravel-dispatch' => \Bench\Scenarios\LaravelDispatchBenchmark::class,
    'laravel-worker' => \Bench\Scenarios\LaravelWorkerBenchmark::class,
];

$budgetPath = __DIR__ . '/../baselines/smoke-budget.json';
$budget = null;
if (is_file($budgetPath)) {
    $budget = new Budget($budgetPath);
}

$allResults = [];

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
            $losses = $stats['consume']['losses'] ?? 0;
            $duplicates = $stats['consume']['duplicates'] ?? 0;
            if ($losses > 0) {
                printf("  WARNING: %d messages lost (published %d, consumed %d)\n",
                    $losses, Config::MESSAGE_COUNT, Config::MESSAGE_COUNT - $losses);
            }
            if ($duplicates > 0) {
                printf("  WARNING: %d duplicate deliveries detected\n", $duplicates);
            }
            if ($budget !== null) {
                $budgetResult = $budget->check($stats['publish'], $stats['consume']);
                echo $budget->formatResult($budgetResult);
            }
        } catch (\Throwable $e) {
            echo "  SKIP: {$e->getMessage()}\n";
        }
    }
}

echo "\n=== Summary ===\n";
printf("%-30s | %-15s | %-15s | %-10s | %-10s | %-10s\n", "Scenario/Driver", "Publish msg/s", "Consume msg/s", "p99 (ms)", "Losses", "Duplicates");
echo str_repeat('-', 105) . "\n";
foreach ($allResults as $key => $stats) {
    printf("%-30s | %-15.0f | %-15.0f | %-10.2f | %-10d | %-10d\n",
        $key,
        $stats['publish']['avg_rate'],
        $stats['consume']['avg_rate'],
        $stats['publish']['p99'],
        $stats['consume']['losses'] ?? 0,
        $stats['consume']['duplicates'] ?? 0
    );
}
echo "\n";

// Write JSON results
$resultsFile = __DIR__ . '/../results/benchmark-results.json';
@mkdir(dirname($resultsFile), recursive: true);
file_put_contents($resultsFile, json_encode($allResults, JSON_PRETTY_PRINT));
echo "\nResults written to {$resultsFile}\n";
