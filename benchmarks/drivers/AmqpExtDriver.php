<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\Metrics;
use RuntimeException;

class AmqpExtDriver implements Driver
{
    use Metrics;

    private const EXCHANGE = 'bench.amqpext';
    private const QUEUE = 'bench.amqpext';

    private $connection = null;
    private $channel = null;
    private $exchange = null;
    private $queue = null;
    private int $publishCount = 0;
    private int $consumeCount = 0;
    private int $losses = 0;

    public function __construct()
    {
        if (!extension_loaded('amqp')) {
            throw new DriverUnavailableException('The pecl "amqp" extension is not loaded');
        }
    }

    public function setup(): void
    {
        $this->connection = new \AMQPConnection([
            'host' => '127.0.0.1',
            'port' => 5672,
            'login' => 'admin',
            'password' => 'admin_lab',
            'vhost' => '/',
        ]);
        $this->connection->connect();

        $this->channel = new \AMQPChannel($this->connection);

        $this->exchange = new \AMQPExchange($this->channel);
        $this->exchange->setName(self::EXCHANGE);
        $this->exchange->setType(AMQP_EX_TYPE_DIRECT);
        $this->exchange->setFlags(AMQP_DURABLE);
        $this->exchange->declareExchange();

        $this->queue = new \AMQPQueue($this->channel);
        $this->queue->setName(self::QUEUE);
        $this->queue->setFlags(AMQP_DURABLE);
        $this->queue->declareQueue();
        $this->queue->bind($this->exchange->getName(), self::QUEUE);

        $this->channel->setPrefetchCount(16);
    }

    public function publish(array $messages, string $safety = 'safest'): void
    {
        if ($this->exchange === null || $this->channel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $useConfirms = ($safety !== 'unsafe');
        $mandatory = ($safety === 'safest');

        if ($useConfirms) {
            $this->channel->confirmSelect();
        }

        $flags = $mandatory ? AMQP_MANDATORY : AMQP_NOPARAM;

        $this->returns = 0;
        $this->losses = 0;

        foreach ($messages as $payload) {
            $ts = hrtime(true);
            $attrs = [
                'message_id' => $this->uuid(),
                'delivery_mode' => AMQP_DURABLE,
            ];
            $this->exchange->publish(pack('P', $ts) . $payload, self::QUEUE, $flags, $attrs);
        }

        if ($useConfirms) {
            $ok = $this->channel->waitForConfirms(5);
            if (!$ok) {
                $this->losses++;
            }
        }

        if ($mandatory) {
            try {
                $this->channel->waitForConfirmsOrDie(1);
            } catch (\AMQPException) {
            }
        }

        $this->publishCount += count($messages);
    }

    public function consume(int $count): void
    {
        if ($this->queue === null) {
            throw new RuntimeException('Driver not set up');
        }

        $this->consumeCount = 0;
        $this->losses = 0;

        $consecutiveNulls = 0;
        while ($this->consumeCount < $count) {
            $envelope = $this->queue->get(AMQP_NOPARAM);
            if ($envelope === false) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 3) {
                    break;
                }
                usleep(1000);
                continue;
            }
            $consecutiveNulls = 0;

            $body = $envelope->getBody();
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $this->queue->ack($envelope->getDeliveryTag());
            $this->consumeCount++;
        }

        $this->losses = max(0, $count - $this->consumeCount);
    }

    public function reset(): void
    {
        if ($this->queue !== null) {
            try {
                $this->queue->purge();
            } catch (\Throwable) {
            }
        }
        $this->resetLatencies();
    }

    public function teardown(): void
    {
        try {
            if ($this->connection !== null && $this->connection->isConnected()) {
                $this->connection->disconnect();
            }
        } catch (\Throwable) {
        }
        $this->connection = null;
        $this->channel = null;
        $this->exchange = null;
        $this->queue = null;
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
        return 'amqp-ext';
    }

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
