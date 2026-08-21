<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\Config;
use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Pool;
use RuntimeException;

class RabbitRsDriver extends AbstractBenchmark
{
    private const QUEUE = 'bench.rabbit-rs';

    private ?Pool $pool = null;
    private ?Consumer $consumer = null;

    public function getName(): string
    {
        return 'rabbit-rs';
    }

    public function setUp(): void
    {
        $config = [
            'brokers' => [[
                'name' => 'default',
                'hosts' => [['host' => Config::RABBITMQ_HOST, 'port' => Config::RABBITMQ_PORT]],
                'vhost' => Config::RABBITMQ_VHOST,
                'credentials' => ['username' => Config::RABBITMQ_USER, 'password' => Config::RABBITMQ_PASSWORD],
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
                    'prefetch' => 64,
                ]],
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 256,
                ],
            ]],
            'topology_mode' => 'declare',
        ];

        $this->pool = new Pool($config);
    }

    public function publishMessages(int $count): void
    {
        if ($this->pool === null) {
            throw new RuntimeException('Driver not set up');
        }

        $batch = [];
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $batch[] = [
                'broker' => 'default',
                'exchange' => '',
                'routing_key' => self::QUEUE,
                'payload' => pack('P', $ts) . $this->createMessage((string) $i),
                'message_id' => $this->uuid(),
                'timeout_ms' => 30000,
            ];

            if (count($batch) >= 256) {
                $this->pool->publishBatch($batch);
                $batch = [];
            }
        }

        if ($batch !== []) {
            $this->pool->publishBatch($batch);
        }
    }

    public function consumeMessages(int $count): void
    {
        if ($this->pool === null) {
            throw new RuntimeException('Driver not set up');
        }

        $this->consumer = $this->pool->consumer('default');

        $consumed = 0;
        $consecutiveNulls = 0;
        while ($consumed < $count) {
            $delivery = $this->consumer->tryNext();
            if ($delivery === null) {
                $delivery = $this->consumer->next(1000);
                if ($delivery === null) {
                    $consecutiveNulls++;
                    if ($consecutiveNulls >= 3) {
                        break;
                    }
                    continue;
                }
            }
            $consecutiveNulls = 0;

            $payload = $delivery->payload();
            if (strlen($payload) >= 8) {
                $ts = unpack('P', substr($payload, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }

            $delivery->ack();
            $consumed++;
        }
    }

    public function tearDown(): void
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

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
