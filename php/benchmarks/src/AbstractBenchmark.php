<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;
use Bunny\Client;

abstract class AbstractBenchmark
{
    protected $connection;
    protected $channel;

    abstract public function getName(): string;
    abstract public function setUp();
    abstract public function tearDown();
    abstract public function publishMessages(int $count);
    abstract public function consumeMessages(int $count);

    protected function createMessage(string $body): string
    {
        return json_encode([
            'id' => uniqid(),
            'timestamp' => microtime(true),
            'data' => $body,
            'payload' => str_repeat('x', Config::MESSAGE_PAYLOAD_BYTES)
        ]);
    }

    public function runBenchmark(): array
    {
        $results = [];

        // Warmup rounds
//         for ($i = 0; $i < Config::WARMUP_ROUNDS; $i++) {
//             $this->publishMessages(100);
//             $this->consumeMessages(100);
//         }

        // Actual benchmark rounds
        for ($i = 0; $i < Config::BENCHMARK_ROUNDS; $i++) {
            // Publish benchmark
            $start = microtime(true);
            $this->publishMessages(Config::MESSAGE_COUNT);
            $publishTime = microtime(true) - $start;

            // Consume benchmark
            $start = microtime(true);
            $this->consumeMessages(Config::MESSAGE_COUNT);
            $consumeTime = microtime(true) - $start;

            $results[] = [
                'publish_time' => $publishTime,
                'consume_time' => $consumeTime,
                'messages_per_second_publish' => Config::MESSAGE_COUNT / $publishTime,
                'messages_per_second_consume' => Config::MESSAGE_COUNT / $consumeTime
            ];
        }

        return $this->calculateStats($results);
    }

    private function calculateStats(array $results): array
    {
        $publishTimes = array_column($results, 'publish_time');
        $consumeTimes = array_column($results, 'consume_time');
        $publishRates = array_column($results, 'messages_per_second_publish');
        $consumeRates = array_column($results, 'messages_per_second_consume');

        return [
            'name' => $this->getName(),
            'publish' => [
                'avg_time' => array_sum($publishTimes) / count($publishTimes),
                'min_time' => min($publishTimes),
                'max_time' => max($publishTimes),
                'avg_rate' => array_sum($publishRates) / count($publishRates),
                'min_rate' => min($publishRates),
                'max_rate' => max($publishRates)
            ],
            'consume' => [
                'avg_time' => array_sum($consumeTimes) / count($consumeTimes),
                'min_time' => min($consumeTimes),
                'max_time' => max($consumeTimes),
                'avg_rate' => array_sum($consumeRates) / count($consumeRates),
                'min_rate' => min($consumeRates),
                'max_rate' => max($consumeRates)
            ]
        ];
    }
}
