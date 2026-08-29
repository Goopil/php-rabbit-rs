<?php

declare(strict_types=1);

namespace Bench\Laravel;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;

class LaravelCompareBenchmark extends AbstractBenchmark
{
    private const QUEUE_PREFIX = 'bench-laravel-compare';
    private string $queueName;
    private $driver;
    private $queue = null;

    public function getName(): string
    {
        return 'laravel-compare-' . ($this->driver ?? 'unknown');
    }

    public function setDriver(string $driver): void
    {
        $this->driver = $driver;
    }

    public function setUp(): void
    {
        $this->queueName = self::QUEUE_PREFIX . '-' . uniqid('', true);

        $config = $this->config($this->queueName);

        $app = new \Illuminate\Container\Container();
        $app->instance('config', new \Illuminate\Config\Repository([
            'queue' => [
                'connections' => [
                    'rabbit-rs-bench' => $config,
                ],
            ],
        ]));

        $factory = new \Illuminate\Queue\QueueManager($app);
        $this->queue = $factory->connection('rabbit-rs-bench');
        $this->queue->setContainer($app);

        try {
            $this->queue->clear($this->queueName);
        } catch (\Throwable) {
            // Queue may not exist yet; safe to ignore.
        }
    }

    public function publishMessages(int $count): void
    {
        for ($i = 0; $i < $count; $i++) {
            $this->queue->push('stdClass', ['index' => $i], $this->queueName);
        }
    }

    public function consumeMessages(int $count): void
    {
        $received = 0;
        $consecutiveNulls = 0;

        while ($received < $count) {
            $job = $this->queue->pop($this->queueName);
            if ($job === null) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 5) {
                    break;
                }
                continue;
            }
            $consecutiveNulls = 0;
            $job->delete();
            $received++;
        }
    }

    public function tearDown(): void
    {
        if ($this->queue !== null) {
            try {
                $this->queue->clear($this->queueName);
            } catch (\Throwable) {
                // Best-effort: ignore errors during cleanup/teardown.
            }
        }
    }

    private function config(string $queueName): array
    {
        return match ($this->driver) {
            'rabbit-rs' => [
                'driver' => 'rabbit-rs',
                'queue' => $queueName,
                'topology_mode' => 'declare',
                'brokers' => [
                    'default' => [
                        'hosts' => ['127.0.0.1:5672'],
                        'vhost' => '/orders-eu',
                        'credentials' => [
                            'username' => 'rabbit_rs',
                            'password' => 'rabbit_rs_lab',
                        ],
                        'tls' => ['enabled' => false, 'server_name' => null],
                        'heartbeat' => 30,
                    ],
                ],
                'routes' => [
                    'default' => [
                        'broker' => 'default',
                        'exchange' => '',
                        'routing_key' => '{queue}',
                    ],
                ],
                'workers' => [
                    'default' => [
                        'scheduler' => [
                            'strategy' => 'weighted_fair',
                        ],
                        'subscriptions' => [
                            'default' => [
                                'enabled' => true,
                                'broker' => 'default',
                                'queue' => $queueName,
                                'weight' => 1,
                                'priority_class' => 0,
                                'prefetch' => ['mode' => 'fixed', 'value' => 16],
                                'starvation_after' => 30,
                            ],
                        ],
                    ],
                ],
                'publisher' => [
                    'confirms' => true,
                    'mandatory' => true,
                ],
            ],
            'php-amqplib' => [
                'driver' => 'rabbitmq',
                'queue' => $queueName,
                'host' => '127.0.0.1',
                'port' => 5672,
                'user' => 'rabbit_rs',
                'password' => 'rabbit_rs_lab',
                'vhost' => '/orders-eu',
                'prefetch_count' => 16,
            ],
            default => throw new BenchmarkException("Unknown driver: {$this->driver}"),
        };
    }
}
