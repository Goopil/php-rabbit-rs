#!/usr/bin/env php
<?php

require_once __DIR__ . '/../vendor/autoload.php';
require_once __DIR__ . '/Config.php';

use Goopil\RabbitRs\Benchmarks;

$includeRabbitRs = class_exists('Goopil\\RabbitRs\\PhpClient');
$includeAmqpExt = extension_loaded('amqp');
$argvCopy = array_slice($argv, 1);

$scenarioFilter = null;
foreach ($argvCopy as $arg) {
    if (strpos($arg, '--scenario=') === 0) {
        $scenarioFilter = substr($arg, strlen('--scenario='));
    }
}

echo "Starting RabbitMQ Client Benchmarks\n";
echo "====================================\n\n";

if (!$includeRabbitRs) {
    echo "Skipping RabbitRs scenarios (extension not loaded).\n";
}

if (!$includeAmqpExt) {
    echo "Skipping php-amqp extension scenarios (ext-amqp not loaded).\n";
}

$benchmarksByScenario = [
    'fire-and-forget' => [],
    'batch-confirm' => [],
    'auto-ack' => [],
    'auto-qos' => [],
];

if ($includeRabbitRs) {
    $benchmarksByScenario['fire-and-forget'][] = new Benchmarks\RabbitRsFireAndForgetBenchmark();
    $benchmarksByScenario['batch-confirm'][] = new Benchmarks\RabbitRsBatchConfirmBenchmark();
    $benchmarksByScenario['auto-ack'][] = new Benchmarks\RabbitRsAutoAckBenchmark();
}

if ($includeAmqpExt) {
    $benchmarksByScenario['fire-and-forget'][] = new Benchmarks\PhpAmqpExtFireAndForgetBenchmark();
    $benchmarksByScenario['batch-confirm'][] = new Benchmarks\PhpAmqpExtBatchConfirmBenchmark();
    $benchmarksByScenario['auto-ack'][] = new Benchmarks\PhpAmqpExtAutoAckBenchmark();
}

$benchmarksByScenario['fire-and-forget'][] = new Benchmarks\AmqplibFireAndForgetBenchmark();
$benchmarksByScenario['batch-confirm'][] = new Benchmarks\AmqplibBatchConfirmBenchmark();
$benchmarksByScenario['auto-ack'][] = new Benchmarks\AmqplibAutoAckBenchmark();

$benchmarksByScenario['fire-and-forget'][] = new Benchmarks\BunnyFireAndForgetBenchmark();
$benchmarksByScenario['batch-confirm'][] = new Benchmarks\BunnyBatchConfirmBenchmark();
$benchmarksByScenario['auto-ack'][] = new Benchmarks\BunnyAutoAckBenchmark();

$resultsByScenario = [];

foreach ($benchmarksByScenario as $scenario => $benchmarks) {
    if ($scenarioFilter && $scenario !== $scenarioFilter) {
        continue;
    }

    if (empty($benchmarks)) {
        echo "Skipping scenario '{$scenario}' (no eligible benchmarks).\n\n";
        continue;
    }

    $label = ucwords(str_replace('-', ' ', $scenario));
    echo "Scenario: {$label}\n";
    echo str_repeat('-', 40) . "\n";

    foreach ($benchmarks as $benchmark) {
        echo "Running benchmark for: " . $benchmark->getName() . "\n";

        try {
            $benchmark->setUp();
            $result = $benchmark->runBenchmark();
            $resultsByScenario[$scenario][] = $result;
            $benchmark->tearDown();

            printf("Publish: %.2f msgs/sec (avg)\n", $result['publish']['avg_rate']);
            printf("Consume: %.2f msgs/sec (avg)\n", $result['consume']['avg_rate']);
            echo "\n";
        } catch (\Throwable $e) {
            echo "Error running benchmark for " . $benchmark->getName() . ": " . $e->getMessage() . "\n\n";
        }
    }
}

if ($scenarioFilter && !isset($resultsByScenario[$scenarioFilter])) {
    echo "No benchmarks executed for scenario '{$scenarioFilter}'. Use --scenario=<name> with one of: fire-and-forget, batch-confirm, auto-ack, auto-qos.\n";
    exit(1);
}

foreach ($resultsByScenario as $scenario => $results) {
    echo "Benchmark Comparison Report ({$scenario})\n";
    echo "==========================\n";

    if (count($results) < 2) {
        echo "Not enough benchmarks to generate comparison.\n\n";
        continue;
    }

    usort($results, function ($a, $b) {
        return $b['publish']['avg_rate'] <=> $a['publish']['avg_rate'];
    });

    echo "Publish Performance Ranking:\n";
    foreach ($results as $i => $result) {
        printf("%d. %s: %.2f msgs/sec\n", $i + 1, $result['name'], $result['publish']['avg_rate']);
    }

    echo "\n";

    usort($results, function ($a, $b) {
        return $b['consume']['avg_rate'] <=> $a['consume']['avg_rate'];
    });

    echo "Consume Performance Ranking:\n";
    foreach ($results as $i => $result) {
        printf("%d. %s: %.2f msgs/sec\n", $i + 1, $result['name'], $result['consume']['avg_rate']);
    }

    echo "\n";
}
