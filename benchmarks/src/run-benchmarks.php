#!/usr/bin/env php
<?php

declare(strict_types=1);

const BASE_DIR = __DIR__ . '/..';

require_once BASE_DIR . '/vendor/autoload.php';

use Bench\AbstractBenchmark;
use Bench\Budget;
use Bench\Config;
use Bench\Drivers;
use Bench\ResultMeta;
use Bench\ScenarioMode;

/**
 * Wraps a driver in the scenario decorator previously provided by the
 * Bench\Scenarios classes: applies the scenario mode, the optional payload
 * size override, and a labelled name, then delegates everything else.
 */
function decorate_scenario(
    AbstractBenchmark $driver,
    string $mode,
    string $label,
    ?int $payloadBytes = null,
): AbstractBenchmark {
    return new class($driver, $mode, $label, $payloadBytes) extends AbstractBenchmark {
        public function __construct(
            private readonly AbstractBenchmark $driver,
            string $mode,
            private readonly string $label,
            ?int $payloadBytes,
        ) {
            $driver->setScenarioMode($mode);
            if ($payloadBytes !== null) {
                $driver->payloadBytes = $payloadBytes;
            }
        }

        public function getName(): string
        {
            return $this->driver->getName() . " ({$this->label})";
        }

        public function setUp(): void { $this->driver->setUp(); }
        public function tearDown(): void { $this->driver->tearDown(); }
        public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
        public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
        public function purgeQueue(): void { $this->driver->purgeQueue(); }
        public function runBenchmark(): array { return $this->driver->runBenchmark(); }
    };
}

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
    'fire-and-forget' => ScenarioMode::FIRE_AND_FORGET,
    'batch-confirm' => ScenarioMode::BATCH_CONFIRM,
    'auto-ack' => ScenarioMode::AUTO_ACK,
    'laravel-dispatch' => ScenarioMode::LARAVEL_DISPATCH,
    'laravel-worker' => ScenarioMode::LARAVEL_WORKER,
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

foreach ($scenarios as $scenarioName => $scenarioMode) {
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
            $payloadBytes = str_starts_with($scenarioName, 'laravel-')
                ? Config::MESSAGE_PAYLOAD_LARAVEL_BYTES
                : null;
            $benchmark = decorate_scenario($driver, $scenarioMode, $scenarioName, $payloadBytes);
            $benchmark->setUp();
            $stats = $benchmark->runBenchmark();
            $benchmark->tearDown();

            $allResults[$scenarioName . '/' . $driverName] = $stats + [
                'config' => ResultMeta::config($payloadBytes ?? Config::MESSAGE_PAYLOAD_BYTES),
                'meta' => ResultMeta::meta(),
            ];

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
