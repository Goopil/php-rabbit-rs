#!/usr/bin/env php
<?php

require_once __DIR__ . '/../vendor/autoload.php';
require_once __DIR__ . '/Config.php';

use Goopil\RabbitRs\Benchmarks;

// Check if we should include RabbitRs benchmark
$includeRabbitRs = class_exists('Goopil\\RabbitRs\\PhpClient');

echo "Starting RabbitMQ Client Benchmarks\n";
echo "====================================\n\n";

$benchmarks = [];

// Conditionally add RabbitRs benchmark
if ($includeRabbitRs) {
    $benchmarks[] = new Benchmarks\RabbitRsBatchConfirmBenchmark();
    $benchmarks[] = new Benchmarks\RabbitRsFireAndForgetBenchmark();
}

$benchmarks[] = new Benchmarks\AmqplibFireAndForgetBenchmark();
$benchmarks[] = new Benchmarks\AmqplibBatchConfirmBenchmark();
$benchmarks[] = new Benchmarks\BunnyFireAndForgetBenchmark();
$benchmarks[] = new Benchmarks\BunnyBatchConfirmBenchmark();

$results = [];

foreach ($benchmarks as $benchmark) {
    echo "Running benchmark for: " . $benchmark->getName() . "\n";

    try {
        $benchmark->setUp();
        $result = $benchmark->runBenchmark();
        $results[] = $result;
        $benchmark->tearDown();

        printf("Publish: %.2f msgs/sec (avg)\n", $result['publish']['avg_rate']);
        printf("Consume: %.2f msgs/sec (avg)\n", $result['consume']['avg_rate']);
        echo "\n";
    } catch (Exception $e) {
        echo "Error running benchmark for " . $benchmark->getName() . ": " . $e->getMessage() . "\n\n";
    }
}

// Generate comparison report
echo "Benchmark Comparison Report\n";
echo "==========================\n\n";

if (count($results) > 1) {
    usort($results, function($a, $b) {
        return $b['publish']['avg_rate'] <=> $a['publish']['avg_rate'];
    });

    echo "Publish Performance Ranking:\n";
    foreach ($results as $i => $result) {
        printf("%d. %s: %.2f msgs/sec\n", $i+1, $result['name'], $result['publish']['avg_rate']);
    }

    echo "\n";

    usort($results, function($a, $b) {
        return $b['consume']['avg_rate'] <=> $a['consume']['avg_rate'];
    });

    echo "Consume Performance Ranking:\n";
    foreach ($results as $i => $result) {
        printf("%d. %s: %.2f msgs/sec\n", $i+1, $result['name'], $result['consume']['avg_rate']);
    }
} else {
    echo "Not enough benchmarks to generate comparison.\n";
}
