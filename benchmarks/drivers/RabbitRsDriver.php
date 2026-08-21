<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\Metrics;
use Goopil\RabbitRs\Pool;
use Goopil\RabbitRs\Consumer;
use RuntimeException;

class RabbitRsDriver implements Driver
{
    use Metrics;

    private const QUEUE = 'bench.rabbit-rs';

    private ?Pool $pool = null;
    private ?Consumer $consumer = null;
    private int $publishCount = 0;
    private int $consumeCount = 0;
    private int $losses = 0;

    public function setup(): void
    {
        $config = [
            'brokers' => [[
                'name' => 'default',
                'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
                'vhost' => '/',
                'credentials' => ['username' => 'admin', 'password' => 'admin_lab'],
                'tls' => ['enabled' => false, 'server_name' => null],
                'heartbeat' => 30,
            ]],
            'workers' => [[
                'name' => 'default',
                'subscriptions' => [[
                    'name' => 'default',
                    'broker' => 'default',
                    'queue' => self::QUEUE,
                    'weight' => 1,
                    'priority_class' => 0,
                    'prefetch' => 512,
                ]],
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 2048,
                ],
            ]],
            'topology_mode' => 'declare',
        ];

        $this->pool = new Pool($config);
    }

    public function publish(array $messages, string $safety = 'safest'): void
    {
        if ($this->pool === null) {
            throw new RuntimeException('Driver not set up');
        }

        $timeoutMs = match ($safety) {
            'unsafe' => 100,
            'confirms' => 5000,
            'safest' => 30000,
            default => 30000,
        };

        $batch = [];
        foreach ($messages as $msg) {
            $ts = hrtime(true);
            $batch[] = [
                'broker' => 'default',
                'exchange' => '',
                'routing_key' => self::QUEUE,
                'payload' => pack('P', $ts) . $msg,
                'message_id' => $this->uuid(),
                'timeout_ms' => $timeoutMs,
            ];

            if (count($batch) >= 256) {
                $this->pool->publishBatch($batch);
                $batch = [];
            }
        }

        if ($batch !== []) {
            $this->pool->publishBatch($batch);
        }

        $this->publishCount += count($messages);
    }

    public function consume(int $count): void
    {
        if ($this->pool === null) {
            throw new RuntimeException('Driver not set up');
        }

        $this->consumer = $this->pool->consumer('default');
        $this->consumeCount = 0;
        $this->losses = 0;

        $processed = $this->consumer->consume(function ($delivery) {
            $payload = $delivery->payload();
            if (strlen($payload) >= 8) {
                $ts = unpack('P', substr($payload, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $delivery->ack();
            return true;
        }, $count, 30000);

        $this->consumeCount = $processed;
        $this->losses = max(0, $count - $this->consumeCount);
    }

    public function reset(): void
    {
        if ($this->consumer !== null) {
            $this->consumer->close();
            $this->consumer = null;
        }
        if ($this->pool !== null) {
            try {
                $this->pool->clear('default', self::QUEUE);
            } catch (\Throwable) {
            }
        }
        $this->resetLatencies();
    }

    public function teardown(): void
    {
        if ($this->consumer !== null) {
            try {
                $this->consumer->close();
            } catch (\Throwable) {
            }
            $this->consumer = null;
        }
        if ($this->pool !== null) {
            $this->pool->close();
            $this->pool = null;
        }
    }

    public function metrics(): array
    {
        $elapsed = $this->elapsedSeconds();
        return $this->buildMetrics(
            $this->consumeCount > 0 ? $this->consumeCount : $this->publishCount,
            $elapsed,
            connections: 1,
            channels: 1,
            losses: $this->losses,
        );
    }

    public function name(): string
    {
        return 'rabbit-rs';
    }

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
