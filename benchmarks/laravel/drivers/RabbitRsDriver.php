<?php

declare(strict_types=1);

namespace Drivers;

use Drivers\Support\MeasuresResources;
use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Pool;

final class RabbitRsDriver implements BenchmarkDriver
{
    use MeasuresResources;

    private ?Pool $pool = null;
    private ?Consumer $consumer = null;
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
        $nativeConfig = $this->config['rabbit-rs-config'] ?? [];
        if ($nativeConfig === []) {
            $fallback = require __DIR__ . '/../config/benchmark.php';
            $nativeConfig = $fallback['rabbit-rs-config'] ?? [];
        }
        try {
            $this->pool = new Pool($nativeConfig);
        } catch (NativeException) {
            $this->pool = null;
        }
    }

    public function publish(array $messages): void
    {
        if ($this->pool === null) {
            return;
        }
        $batch = [];
        $queue = $this->config['queue'] ?? 'bench.rabbit-rs';
        $exchange = $this->config['exchange'] ?? '';
        foreach ($messages as $message) {
            $batch[] = [
                'broker' => 'default',
                'exchange' => $exchange,
                'routing_key' => $queue,
                'payload' => $message,
                'message_id' => $this->uuid(),
            ];
        }
        try {
            $this->pool->publishBatch($batch);
        } catch (NativeException) {
        }
    }

    public function consume(int $count): void
    {
        if ($this->pool === null) {
            return;
        }
        try {
            $this->consumer = $this->pool->consumer('default');
        } catch (NativeException) {
            return;
        }

        $this->consumed = 0;
        $this->seenIds = [];
        $this->duplicates = 0;
        $this->startTimer();

        while ($this->consumed < $count) {
            try {
                $delivery = $this->consumer->next(1000);
            } catch (NativeException) {
                break;
            }
            if ($delivery === null) {
                break;
            }
            $this->processDelivery($delivery);
            $this->consumed++;
        }

        $this->losses = max(0, $count - $this->consumed);
    }

    private function processDelivery(Delivery $delivery): void
    {
        $metadata = $delivery->metadata();
        $id = $metadata['message_id'] ?? null;
        if ($id !== null) {
            if (isset($this->seenIds[$id])) {
                $this->duplicates++;
            }
            $this->seenIds[$id] = true;
        }
        try {
            $delivery->ack();
        } catch (NativeException) {
        }
    }

    public function reset(): void
    {
        if ($this->consumer !== null) {
            try {
                $this->consumer->close();
            } catch (NativeException) {
            }
            $this->consumer = null;
        }
        $this->pool?->clear('default', $this->config['queue'] ?? 'bench.rabbit-rs');
        $this->consumed = 0;
        $this->duplicates = 0;
        $this->losses = 0;
        $this->seenIds = [];
        $this->resetLatencies();
    }

    public function metrics(): array
    {
        $stats = [];
        if ($this->pool !== null) {
            try {
                $stats = $this->pool->stats();
            } catch (NativeException) {
            }
        }

        return $this->buildMetrics(
            messageCount: $this->consumed,
            elapsedSeconds: $this->elapsedSeconds(),
            connections: (int) ($stats['connections'] ?? 0),
            channels: (int) ($stats['channels'] ?? 0),
            duplicates: $this->duplicates,
            losses: $this->losses,
        );
    }

    private function uuid(): string
    {
        $data = random_bytes(16);
        $data[6] = chr((ord($data[6]) & 0x0f) | 0x40);
        $data[8] = chr((ord($data[8]) & 0x3f) | 0x80);

        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($data), 4));
    }
}
