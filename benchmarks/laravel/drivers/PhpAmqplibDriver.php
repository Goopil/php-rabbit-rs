<?php

declare(strict_types=1);

namespace Drivers;

use Drivers\Support\MeasuresResources;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;

final class PhpAmqplibDriver implements BenchmarkDriver
{
    use MeasuresResources;

    private ?AMQPStreamConnection $connection = null;
    private $channel = null;
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
            $this->connection = new AMQPStreamConnection(
                $this->config['host'] ?? '127.0.0.1',
                $this->config['port'] ?? 5672,
                $this->config['user'] ?? 'guest',
                $this->config['password'] ?? 'guest',
                $this->config['vhost'] ?? '/',
            );
            $this->channel = $this->connection->channel();
            $queue = $this->config['queue'] ?? 'bench.amqplib';
            $exchange = $this->config['exchange'] ?? 'bench.amqplib';
            $this->channel->exchange_declare($exchange, 'direct', false, true, false);
            $this->channel->queue_declare($queue, false, true, false, false);
            $this->channel->queue_bind($queue, $exchange, $queue);
            $this->channel->basic_qos(null, 16, null);
        } catch (\Throwable) {
            $this->connection = null;
            $this->channel = null;
        }
    }

    public function publish(array $messages): void
    {
        if ($this->channel === null) {
            return;
        }
        $exchange = $this->config['exchange'] ?? 'bench.amqplib';
        $queue = $this->config['queue'] ?? 'bench.amqplib';
        foreach ($messages as $message) {
            $msg = new AMQPMessage(
                $message,
                [
                    'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                    'message_id' => $this->uuid(),
                ],
            );
            try {
                $this->channel->basic_publish($msg, $exchange, $queue);
            } catch (\Throwable) {
            }
        }
    }

    public function consume(int $count): void
    {
        if ($this->channel === null) {
            return;
        }
        $queue = $this->config['queue'] ?? 'bench.amqplib';
        $this->consumed = 0;
        $this->seenIds = [];
        $this->duplicates = 0;

        $callback = function ($message): void {
            $start = microtime(true);
            $this->latencies[] = (microtime(true) - $start) * 1000;
            $msgId = $message->get('message_id');
            if ($msgId !== null) {
                if (isset($this->seenIds[$msgId])) {
                    $this->duplicates++;
                }
                $this->seenIds[$msgId] = true;
            }
            $message->ack();
            $this->consumed++;
        };

        try {
            $this->channel->basic_consume($queue, '', false, false, false, false, $callback);
            while ($this->consumed < $count && count($this->channel->callbacks()) > 0) {
                $this->channel->wait(null, false, 1);
            }
        } catch (\Throwable) {
        }

        $this->losses = max(0, $count - $this->consumed);
    }

    public function reset(): void
    {
        if ($this->channel !== null) {
            try {
                $queue = $this->config['queue'] ?? 'bench.amqplib';
                $this->channel->queue_purge($queue);
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
            elapsedSeconds: 1.0,
            connections: $this->connection !== null ? 1 : 0,
            channels: $this->channel !== null ? 1 : 0,
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
