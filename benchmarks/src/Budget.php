<?php

declare(strict_types=1);

namespace Bench;

class Budget
{
    /** @var array<string, int|float> */
    private array $budget;

    public function __construct(string $path)
    {
        $contents = @file_get_contents($path);
        if ($contents === false) {
            throw new \RuntimeException("Budget file not found: {$path}");
        }
        $decoded = json_decode($contents, true);
        if (!is_array($decoded)) {
            throw new \RuntimeException("Invalid budget JSON in: {$path}");
        }
        $this->budget = $decoded;
    }

    /**
     * @param array<string, float|int> $publishMetrics
     * @param array<string, float|int> $consumeMetrics
     * @return array{pass: bool, failures: array<array{metric: string, expected: string, actual: string}>}
     */
    public function check(array $publishMetrics, array $consumeMetrics): array
    {
        $failures = [];

        foreach ($this->budget as $key => $expected) {
            $result = $this->checkMetric($key, $expected, $publishMetrics, $consumeMetrics);
            if ($result !== null) {
                $failures[] = $result;
            }
        }

        return ['pass' => $failures === [], 'failures' => $failures];
    }

    private function checkMetric(string $key, int|float $expected, array $publishMetrics, array $consumeMetrics): ?array
    {
        $actual = $this->extractMetric($key, $publishMetrics, $consumeMetrics);
        if ($actual === null) {
            return ['metric' => $key, 'expected' => (string) $expected, 'actual' => 'missing'];
        }

        $pass = match (true) {
            str_ends_with($key, '_throughput_min') => $actual >= $expected,
            str_ends_with($key, '_p99_max_ms') => $actual <= $expected,
            $key === 'losses_max' => $actual == 0,
            default => true,
        };

        if ($pass) {
            return null;
        }

        return [
            'metric' => $key,
            'expected' => (string) $expected,
            'actual' => (string) $actual,
        ];
    }

    private function extractMetric(string $key, array $publishMetrics, array $consumeMetrics): ?float
    {
        return match ($key) {
            'publish_throughput_min' => isset($publishMetrics['throughput']) ? (float) $publishMetrics['throughput'] : null,
            'consume_throughput_min' => isset($consumeMetrics['throughput']) ? (float) $consumeMetrics['throughput'] : null,
            'publish_p99_max_ms' => isset($publishMetrics['p99']) ? (float) $publishMetrics['p99'] : null,
            'consume_p99_max_ms' => isset($consumeMetrics['p99']) ? (float) $consumeMetrics['p99'] : null,
            'losses_max' => isset($consumeMetrics['losses']) ? (float) $consumeMetrics['losses'] : null,
            default => null,
        };
    }

    public function budget(): array
    {
        return $this->budget;
    }

    public function formatResult(array $result): string
    {
        if ($result['pass']) {
            return "Budget Check: ALL PASS\n";
        }

        $lines = ["Budget Check: FAIL"];
        foreach ($result['failures'] as $failure) {
            $lines[] = "  {$failure['metric']}: expected {$failure['expected']}, got {$failure['actual']}";
        }

        return implode("\n", $lines) . "\n";
    }
}
