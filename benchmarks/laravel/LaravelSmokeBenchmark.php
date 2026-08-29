<?php

declare(strict_types=1);

namespace Bench\Laravel;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;
use Bench\Config;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

class LaravelSmokeBenchmark extends AbstractBenchmark
{
    private const QUEUE_PREFIX = 'bench-laravel-smoke';
    private string $queueName;
    private ?Pool $pool = null;
    private $queue = null;

    public function getName(): string
    {
        return 'laravel-smoke';
    }

    public function setUp(): void
    {
        if (!extension_loaded('rabbit_rs')) {
            throw new BenchmarkException('ext-rabbit_rs is not loaded');
        }

        $this->queueName = self::QUEUE_PREFIX . '-' . uniqid('', true);
        $this->declareQueue($this->queueName);

        $config = $this->liveConfig($this->queueName);
        $normalized = ConfigNormalizer::normalize($config);

        $this->pool = new Pool($normalized['native']);
        $factory = new NativePoolFactory(createPool: fn (): Pool => $this->pool);
        $connector = new RabbitMqConnector($factory, $normalized);

        $this->queue = $connector->connect([
            'queue' => $this->queueName,
            'block_for' => 3,
        ]);

        $container = new \Illuminate\Container\Container();
        $container->instance('config', new \Illuminate\Config\Repository());
        $this->queue->setContainer($container);
        $this->queue->setConnectionName('rabbit-rs-bench');
        $this->queue->clear($this->queueName);
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
        if ($this->pool !== null) {
            $this->pool->close();
        }
        $this->deleteQueue($this->queueName);
    }

    private function liveConfig(string $queueName): array
    {
        return [
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
            'topology' => [
                'queue' => [
                    'type' => 'quorum',
                    'durable' => true,
                    'delivery_limit' => null,
                ],
                'dead_letter' => null,
            ],
        ];
    }

    private function declareQueue(string $queueName): void
    {
        $url = 'http://localhost:15672/api/queues/%2Forders-eu/' . urlencode($queueName);
        $payload = json_encode([
            'durable' => true,
            'arguments' => ['x-queue-type' => 'quorum', 'x-delivery-limit' => 20],
        ]);

        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
        curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        curl_exec($ch);
        curl_close($ch);
    }

    private function deleteQueue(string $queueName): void
    {
        $url = 'http://localhost:15672/api/queues/%2Forders-eu/' . urlencode($queueName);
        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
        curl_exec($ch);
        curl_close($ch);
    }
}
