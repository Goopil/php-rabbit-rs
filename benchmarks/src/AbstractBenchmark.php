<?php

declare(strict_types=1);

namespace Bench;

abstract class AbstractBenchmark
{
    protected array $latencies = [];

    protected string $scenarioMode = ScenarioMode::BATCH_CONFIRM;

    public function setScenarioMode(string $mode): void
    {
        $this->scenarioMode = $mode;
    }

    abstract public function getName(): string;
    abstract public function setUp(): void;
    abstract public function tearDown(): void;
    abstract public function publishMessages(int $count): void;
    abstract public function consumeMessages(int $count): void;

    public function purgeQueue(): void {}

    private ?string $payloadTemplate = null;
    private int $uuidCounter = 0;

    protected function createMessage(string $body): string
    {
        if ($this->payloadTemplate === null) {
            $this->payloadTemplate = str_repeat('x', Config::MESSAGE_PAYLOAD_BYTES);
        }
        return json_encode([
            'id' => uniqid('', true),
            'timestamp' => microtime(true),
            'data' => $body,
            'payload' => $this->payloadTemplate,
        ]);
    }

    protected function uuid(): string
    {
        return sprintf('00000000-0000-4000-8000-%012d', $this->uuidCounter++);
    }

    public function runBenchmark(): array
    {
        $results = [];
        $gcEnabled = gc_enabled();
        gc_disable();

        $this->latencies = [];
        $this->purgeQueue();
        $this->publishMessages(Config::MESSAGE_COUNT);
        $this->consumeMessages(Config::MESSAGE_COUNT);

        for ($i = 0; $i < Config::BENCHMARK_ROUNDS; $i++) {
            $this->latencies = [];

            $this->purgeQueue();
            $start = microtime(true);
            $this->publishMessages(Config::MESSAGE_COUNT);
            $publishTime = microtime(true) - $start;

            $start = microtime(true);
            $this->consumeMessages(Config::MESSAGE_COUNT);
            $consumeTime = microtime(true) - $start;

            $consumedCount = count($this->latencies);
            $losses = Config::MESSAGE_COUNT - $consumedCount;

            $results[] = [
                'publish_time' => $publishTime,
                'consume_time' => $consumeTime,
                'publish_rate' => Config::MESSAGE_COUNT / $publishTime,
                'consume_rate' => Config::MESSAGE_COUNT / $consumeTime,
                'p50' => $this->percentile(0.50),
                'p95' => $this->percentile(0.95),
                'p99' => $this->percentile(0.99),
                'losses' => $losses,
            ];
        }
        if ($gcEnabled) {
            gc_enable();
        }
        return $this->calculateStats($results);
    }

    protected function recordLatency(float $ms): void
    {
        $this->latencies[] = $ms;
    }

    protected function percentile(float $p): float
    {
        if (empty($this->latencies)) {
            return 0.0;
        }
        $sorted = $this->latencies;
        sort($sorted);
        $index = (int) floor($p * count($sorted));
        return $sorted[min($index, count($sorted) - 1)];
    }

    private function calculateStats(array $results): array
    {
        $get = fn(string $key) => array_column($results, $key);
        $avg = fn(array $vals) => array_sum($vals) / count($vals);

        $publishTimes = $get('publish_time');
        $consumeTimes = $get('consume_time');
        $publishRates = $get('publish_rate');
        $consumeRates = $get('consume_rate');
        $losses = $get('losses');

        return [
            'name' => $this->getName(),
            'publish' => [
                'avg_time' => $avg($publishTimes),
                'min_time' => min($publishTimes),
                'max_time' => max($publishTimes),
                'avg_rate' => $avg($publishRates),
                'min_rate' => min($publishRates),
                'max_rate' => max($publishRates),
                'p99' => $avg($get('p99')),
            ],
            'consume' => [
                'avg_time' => $avg($consumeTimes),
                'min_time' => min($consumeTimes),
                'max_time' => max($consumeTimes),
                'avg_rate' => $avg($consumeRates),
                'min_rate' => min($consumeRates),
                'max_rate' => max($consumeRates),
                'p50' => $avg($get('p50')),
                'p95' => $avg($get('p95')),
                'p99' => $avg($get('p99')),
                'losses' => array_sum($losses),
            ],
        ];
    }
}
