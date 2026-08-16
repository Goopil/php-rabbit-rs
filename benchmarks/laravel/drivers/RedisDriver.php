<?php

declare(strict_types=1);

namespace Drivers;

use Drivers\Support\MeasuresResources;
use Predis\Client;

final class RedisDriver implements BenchmarkDriver
{
    use MeasuresResources;

    private ?Client $redis = null;
    private array $config;
    private int $consumed = 0;
    private array $seenIds = [];
    private int $duplicates = 0;
    private int $losses = 0;

    public function __construct(array $config = [])
    {
        $this->config = $config;
    }

    public function setup(): void
    {
        try {
            $this->redis = new Client([
                'scheme' => 'tcp',
                'host' => $this->config['host'] ?? '127.0.0.1',
                'port' => $this->config['port'] ?? 6379,
            ]);
            $this->redis->ping();
        } catch (\Throwable) {
            $this->redis = null;
        }
    }

    public function publish(array $messages): void
    {
        if ($this->redis === null) {
            return;
        }
        $queue = $this->config['queue'] ?? 'bench.redis';
        foreach ($messages as $message) {
            try {
                $this->redis->rpush($queue, [$message]);
            } catch (\Throwable) {
            }
        }
    }

    public function consume(int $count): void
    {
        if ($this->redis === null) {
            return;
        }
        $queue = $this->config['queue'] ?? 'bench.redis';
        $this->consumed = 0;
        $this->seenIds = [];
        $this->duplicates = 0;
        $this->startTimer();

        while ($this->consumed < $count) {
            try {
                $message = $this->redis->lpop($queue);
            } catch (\Throwable) {
                break;
            }
            if ($message === null) {
                break;
            }
            $payload = json_decode((string) $message, true);
            $msgId = $payload['id'] ?? null;
            if ($msgId !== null) {
                if (isset($this->seenIds[$msgId])) {
                    $this->duplicates++;
                }
                $this->seenIds[$msgId] = true;
            }
            $this->consumed++;
        }

        $this->losses = max(0, $count - $this->consumed);
    }

    public function reset(): void
    {
        if ($this->redis !== null) {
            try {
                $this->redis->del($this->config['queue'] ?? 'bench.redis');
            } catch (\Throwable) {
            }
        }
        $this->consumed = 0;
        $this->duplicates = 0;
        $this->losses = 0;
        $this->seenIds = [];
        $this->resetLatencies();
    }

    public function metrics(): array
    {
        return $this->buildMetrics(
            messageCount: $this->consumed,
            elapsedSeconds: $this->elapsedSeconds(),
            connections: $this->redis !== null ? 1 : 0,
            channels: 0,
            duplicates: $this->duplicates,
            losses: $this->losses,
        );
    }
}
