<?php

declare(strict_types=1);

namespace Drivers;

use Drivers\Support\MeasuresResources;
use Illuminate\Container\Container;
use Illuminate\Queue\QueueManager;
use VladimirYuldashev\LaravelQueueRabbitMQ\Queue\Connectors\RabbitMQConnector;
use VladimirYuldashev\LaravelQueueRabbitMQ\Queue\RabbitMQQueue;

final class VyuldashevDriver implements BenchmarkDriver
{
    use MeasuresResources;

    private ?RabbitMQQueue $queue = null;
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
            $container = Container::getInstance() ?? new Container();
            if (! $container->bound('config')) {
                $container->instance('config', new \Illuminate\Config\Repository());
            }
            $events = new \Illuminate\Events\Dispatcher($container);
            $queueManager = new QueueManager($events);
            $queueManager->addConnector('rabbitmq', function () use ($events) {
                return new RabbitMQConnector($events);
            });

            $dsn = parse_url($this->config['connection'] ?? 'amqp://guest:guest@127.0.0.1:5672/');
            $config = [
                'driver' => 'rabbitmq',
                'queue' => $this->config['queue'] ?? 'bench.vyuldashev',
                'connection' => 'default',
                'hosts' => [
                    [
                        'host' => $dsn['host'] ?? '127.0.0.1',
                        'port' => $dsn['port'] ?? 5672,
                        'vhost' => isset($dsn['path']) ? ltrim($dsn['path'], '/') : '/',
                        'user' => $dsn['user'] ?? 'guest',
                        'password' => $dsn['pass'] ?? 'guest',
                    ],
                ],
                'options' => [
                    'exchange' => 'bench.vyuldashev',
                    'exchange_type' => 'direct',
                    'exchange_routing_key' => '',
                    'with_queue' => true,
                    'queue_passive' => false,
                    'queue_durable' => true,
                    'queue_exclusive' => false,
                    'queue_auto_delete' => false,
                    'queue_arguments' => [],
                ],
            ];

            $container['config']->set('queue.connections.rabbitmq', $config);
            $container['config']->set('queue.default', 'rabbitmq');

            $this->queue = $queueManager->connection('rabbitmq');
            $this->queue->setContainer($container);
        } catch (\Throwable) {
            $this->queue = null;
        }
    }

    public function publish(array $messages): void
    {
        if ($this->queue === null) {
            return;
        }
        $queue = $this->config['queue'] ?? 'bench.vyuldashev';
        foreach ($messages as $message) {
            try {
                $this->queue->pushRaw($message, $queue);
            } catch (\Throwable) {
            }
        }
    }

    public function consume(int $count): void
    {
        if ($this->queue === null) {
            return;
        }
        $queue = $this->config['queue'] ?? 'bench.vyuldashev';
        $this->consumed = 0;
        $this->seenIds = [];
        $this->duplicates = 0;
        $this->startTimer();

        while ($this->consumed < $count) {
            try {
                $job = $this->queue->pop($queue);
            } catch (\Throwable) {
                break;
            }
            if ($job === null) {
                break;
            }
            $payload = json_decode($job->getRawBody(), true);
            $msgId = $payload['id'] ?? null;
            if ($msgId !== null) {
                if (isset($this->seenIds[$msgId])) {
                    $this->duplicates++;
                }
                $this->seenIds[$msgId] = true;
            }
            try {
                $job->delete();
            } catch (\Throwable) {
            }
            $this->consumed++;
        }

        $this->losses = max(0, $count - $this->consumed);
    }

    public function reset(): void
    {
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
            connections: $this->queue !== null ? 1 : 0,
            channels: $this->queue !== null ? 1 : 0,
            duplicates: $this->duplicates,
            losses: $this->losses,
        );
    }
}
