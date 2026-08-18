<?php

declare(strict_types=1);

use Bench\Budget;
use Bench\Drivers\RabbitRsDriver;

const SMOKE_COUNT = 2000;
const SMOKE_PAYLOAD = 256;
const SMOKE_BATCH = 64;
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

function fail(string $message): never
{
    fwrite(STDERR, "ERROR: {$message}\n");
    exit(1);
}

function formatNumber(float $n): string
{
    return number_format($n, $n >= 100 ? 0 : 1);
}

fwrite(STDOUT, "Rabbit RS Smoke Benchmark\n");
fwrite(STDOUT, "==========================\n\n");

if (!extension_loaded('rabbit_rs')) {
    fail("The 'rabbit_rs' extension is not loaded. Install it before running the smoke benchmark.");
}

fwrite(STDOUT, "Environment:\n");
fwrite(STDOUT, "  PHP: " . PHP_VERSION . "\n");
fwrite(STDOUT, "  SAPI: " . PHP_SAPI . "\n");
fwrite(STDOUT, "  Extension: rabbit_rs loaded\n");
fwrite(STDOUT, "  Broker: 127.0.0.1:5672\n\n");

$driver = new RabbitRsDriver();

try {
    $driver->setup();
} catch (\Throwable $e) {
    fail("Setup failed: " . $e->getMessage());
}

$payload = str_repeat('x', SMOKE_PAYLOAD);

// --- Publish phase ---
$messages = [];
for ($i = 0; $i < SMOKE_COUNT; $i++) {
    $messages[] = $payload;
}

$driver->resetLatencies();
$driver->startTimer();

$batches = array_chunk($messages, SMOKE_BATCH);
foreach ($batches as $batch) {
    $driver->publish($batch, 'safest');
}

$publishElapsed = $driver->elapsedSeconds();
$publishCount = SMOKE_COUNT;
$publishThroughput = $publishElapsed > 0 ? $publishCount / $publishElapsed : 0.0;
$publishP50 = $driver->percentile(50);
$publishP95 = $driver->percentile(95);
$publishP99 = $driver->percentile(99);

fwrite(STDOUT, "Publish (" . SMOKE_COUNT . " msgs, " . SMOKE_PAYLOAD . "B payload, batch " . SMOKE_BATCH . ", safest):\n");
fwrite(STDOUT, "  Throughput: " . formatNumber($publishThroughput) . " msgs/s\n");
fwrite(STDOUT, "  p50: " . formatNumber($publishP50) . " ms\n");
fwrite(STDOUT, "  p95: " . formatNumber($publishP95) . " ms\n");
fwrite(STDOUT, "  p99: " . formatNumber($publishP99) . " ms\n\n");

// --- Consume ---
$driver->resetLatencies();
$driver->startTimer();

try {
    $driver->consume(SMOKE_COUNT);
} catch (\Throwable $e) {
    fail("Consume failed: " . $e->getMessage());
}

$consumeElapsed = $driver->elapsedSeconds();
$consumeThroughput = $consumeElapsed > 0 ? SMOKE_COUNT / $consumeElapsed : 0.0;
$consumeP50 = $driver->percentile(50);
$consumeP95 = $driver->percentile(95);
$consumeP99 = $driver->percentile(99);

fwrite(STDOUT, "Consume (" . SMOKE_COUNT . " msgs):\n");
fwrite(STDOUT, "  Throughput: " . formatNumber($consumeThroughput) . " msgs/s\n");
fwrite(STDOUT, "  p50: " . formatNumber($consumeP50) . " ms\n");
fwrite(STDOUT, "  p95: " . formatNumber($consumeP95) . " ms\n");
fwrite(STDOUT, "  p99: " . formatNumber($consumeP99) . " ms\n\n");

$metrics = $driver->metrics();
$losses = $metrics['losses'];
$duplicates = $metrics['duplicates'];

fwrite(STDOUT, "Losses: {$losses}\n");
fwrite(STDOUT, "Duplicates: {$duplicates}\n\n");

// --- Budget check ---
$budgetPath = BASE_DIR . '/baselines/smoke-budget.json';
try {
    $budget = new Budget($budgetPath);
} catch (\Throwable $e) {
    fail("Could not load budget: " . $e->getMessage());
}

$publishMetrics = [
    'throughput' => $publishThroughput,
    'p99' => $publishP99,
    'losses' => $losses,
];
$consumeMetrics = [
    'throughput' => $consumeThroughput,
    'p99' => $consumeP99,
    'losses' => $losses,
];

$budgetResult = $budget->check($publishMetrics, $consumeMetrics);
$allPass = $budgetResult['pass'];

fwrite(STDOUT, "Budget Check:\n");
$budgetKeys = [
    'publish_throughput_min' => ['value' => $publishThroughput, 'label' => 'publish_throughput'],
    'consume_throughput_min' => ['value' => $consumeThroughput, 'label' => 'consume_throughput'],
    'publish_p99_max_ms' => ['value' => $publishP99, 'label' => 'publish_p99'],
    'consume_p99_max_ms' => ['value' => $consumeP99, 'label' => 'consume_p99'],
    'losses_max' => ['value' => $losses, 'label' => 'losses'],
];
$failedKeys = array_column($budgetResult['failures'], 'metric');
foreach ($budgetKeys as $key => $info) {
    $isMin = str_ends_with($key, '_throughput_min');
    $isMax = str_ends_with($key, '_p99_max_ms');
    $isLosses = $key === 'losses_max';
    $expected = $budget->budget()[$key] ?? null;
    if ($expected === null) continue;
    $actual = $info['value'];
    $pass = $isMin ? $actual >= $expected : ($isMax ? $actual <= $expected : ($isLosses ? $actual == 0 : true));
    $op = $isMin ? '>=' : ($isMax ? '<=' : '==');
    fwrite(STDOUT, sprintf("  %s: %s %s %s ... %s\n", $info['label'], formatNumber($actual), $op, formatNumber($expected), $pass ? 'PASS' : 'FAIL'));
}
fwrite(STDOUT, "\n");

// --- Write results JSON ---
$resultsDir = BASE_DIR . '/results';
if (!is_dir($resultsDir)) {
    mkdir($resultsDir, 0755, true);
}

$timestamp = date('Ymd-His');
$resultsFile = $resultsDir . '/smoke-' . $timestamp . '.json';

$results = [
    'timestamp' => $timestamp,
    'environment' => [
        'php' => PHP_VERSION,
        'sapi' => PHP_SAPI,
        'extension' => 'rabbit_rs',
        'broker' => '127.0.0.1:5672',
    ],
    'parameters' => [
        'count' => SMOKE_COUNT,
        'payload_bytes' => SMOKE_PAYLOAD,
        'batch_size' => SMOKE_BATCH,
        'safety' => 'safest',
    ],
    'publish' => [
        'throughput' => $publishThroughput,
        'p50' => $publishP50,
        'p95' => $publishP95,
        'p99' => $publishP99,
    ],
    'consume' => [
        'throughput' => $consumeThroughput,
        'p50' => $consumeP50,
        'p95' => $consumeP95,
        'p99' => $consumeP99,
    ],
    'losses' => $losses,
    'duplicates' => $duplicates,
    'budget_pass' => $allPass,
];

file_put_contents($resultsFile, json_encode($results, JSON_PRETTY_PRINT));
fwrite(STDOUT, "Results written to: {$resultsFile}\n\n");

fwrite(STDOUT, "Result: " . ($allPass ? 'ALL PASS' : 'REGRESSION DETECTED') . "\n");
$driver->reset();

exit($allPass ? 0 : 1);
