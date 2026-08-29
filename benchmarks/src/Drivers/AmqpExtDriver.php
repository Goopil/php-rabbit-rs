<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;
use Bench\Config;
use Bench\ScenarioMode;

class AmqpExtDriver extends AbstractBenchmark
{
    private const EXCHANGE = 'bench.amqpext';
    private const QUEUE = 'bench.amqpext';

    private $connection = null;
    private $pubChannel = null;
    private $consChannel = null;
    private $pubExchange = null;
    private $consQueue = null;
    private bool $confirmMode = false;

    public function __construct()
    {
        if (!extension_loaded('amqp')) {
            throw new BenchmarkException('The pecl "amqp" extension is not loaded');
        }
    }

    public function getName(): string
    {
        return 'amqp-ext';
    }

    public function setUp(): void
    {
        $this->connection = new \AMQPConnection([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'login' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD,
            'vhost' => Config::RABBITMQ_VHOST,
        ]);
        $this->connection->connect();

        $this->pubChannel = new \AMQPChannel($this->connection);
        $this->consChannel = new \AMQPChannel($this->connection);

        $this->pubExchange = new \AMQPExchange($this->pubChannel);
        $this->pubExchange->setName(self::EXCHANGE);
        $this->pubExchange->setType(AMQP_EX_TYPE_DIRECT);
        $this->pubExchange->setFlags(AMQP_DURABLE);
        $this->pubExchange->declareExchange();

        $this->consQueue = new \AMQPQueue($this->consChannel);
        $this->consQueue->setName(self::QUEUE);
        $this->consQueue->setFlags(AMQP_DURABLE);
        $this->consQueue->declareQueue();
        $this->consQueue->bind($this->pubExchange->getName(), self::QUEUE);

        $this->consChannel->setPrefetchCount(Config::PREFETCH_COUNT);
    }

    public function purgeQueue(): void
    {
        if ($this->consQueue !== null) {
            try {
                $this->consQueue->purge();
            } catch (\Throwable) {
                // Queue may not exist yet; safe to ignore.
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->pubExchange === null || $this->pubChannel === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK
            || $this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
            $this->publishFireAndForget($count);
            return;
        }

        $batchSize = $this->scenarioMode === ScenarioMode::LARAVEL_DISPATCH ? 64 : 256;
        $this->publishWithConfirms($count, $batchSize);
    }

    private function publishFireAndForget(int $count): void
    {
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $attrs = [
                'message_id' => $this->uuid(),
                'delivery_mode' => AMQP_DURABLE,
            ];
            $this->pubExchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_NOPARAM, $attrs);
        }
    }

    private function publishWithConfirms(int $count, int $batchSize): void
    {
        if (!$this->confirmMode) {
            $this->pubChannel->confirmSelect();
            $this->pubChannel->setConfirmCallback(
                function (): bool { return false; },
                function (): bool { return false; },
            );
            $this->confirmMode = true;
        }

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $attrs = [
                'message_id' => $this->uuid(),
                'delivery_mode' => AMQP_DURABLE,
            ];
            $this->pubExchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_NOPARAM, $attrs);

            if (($i + 1) % $batchSize === 0) {
                try {
                    $this->pubChannel->waitForConfirm(5);
                } catch (\Throwable) {
                    break;
                }
            }
        }

        if ($count % $batchSize !== 0) {
            try {
                $this->pubChannel->waitForConfirm(5);
            } catch (\Throwable) {
                // Trailing confirm failure of the final partial batch is not actionable here.
            }
        }
    }

    public function consumeMessages(int $count): void
    {
        if ($this->consQueue === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
            $this->consumeSingleGet($count);
            return;
        }

        if ($this->scenarioMode === ScenarioMode::BATCH_CONFIRM) {
            $this->reconnect();
        }

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;
        $consumerTag = 'bench_amqpext_consumer';
        $flags = $autoAck ? AMQP_AUTOACK : AMQP_NOPARAM;

        $consumed = 0;
        $callback = $this->makeConsumeCallback($count, $autoAck, $consumerTag, $consumed);

        $this->consumeWithTimeouts($this->consQueue, $callback, $count, $consumed, $consumerTag, $flags);
    }

    private function consumeSingleGet(int $count): void
    {
        $consumed = 0;
        $consecutiveEmpty = 0;
        while ($consumed < $count) {
            $envelope = $this->consQueue->get(AMQP_NOPARAM);
            if ($envelope === false) {
                $consecutiveEmpty++;
                if ($consecutiveEmpty >= 3) {
                    break;
                }
                usleep(100_000);
                continue;
            }
            $consecutiveEmpty = 0;

            $body = $envelope->getBody();
            $this->recordReceived($envelope->getMessageId() ?? '');
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $this->consQueue->ack($envelope->getDeliveryTag());
            $consumed++;
        }
    }

    private function makeConsumeCallback(int $count, bool $autoAck, string $consumerTag, int &$consumed): \Closure
    {
        return function (\AMQPEnvelope $envelope, \AMQPQueue $q) use ($count, &$consumed, $autoAck, $consumerTag): bool {
            $body = $envelope->getBody();
            $this->recordReceived($envelope->getMessageId() ?? '');
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $consumed++;
            if (!$autoAck) {
                $q->ack($envelope->getDeliveryTag());
            }
            if ($consumed >= $count) {
                $q->cancel($consumerTag);
                return false;
            }
            return true;
        };
    }

    private function consumeWithTimeouts(\AMQPQueue $queue, \Closure $callback, int $count, int &$consumed, string $consumerTag, int $flags): void
    {
        $consecutiveTimeouts = 0;

        $this->connection->setReadTimeout(1);

        while ($consumed < $count) {
            try {
                $queue->consume($callback, $flags, $consumerTag);
                $consecutiveTimeouts = 0;
            } catch (\AMQPQueueException) {
                $consecutiveTimeouts++;
                if ($consecutiveTimeouts >= 3) {
                    break;
                }
            }
        }

        $this->connection->setReadTimeout(0);
    }

    private function reconnect(): void
    {
        try {
            if ($this->connection !== null && $this->connection->isConnected()) {
                $this->connection->disconnect();
            }
        } catch (\Throwable) {
            // Connection may already be closed; safe to ignore.
        }

        $this->connection = new \AMQPConnection([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'login' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD,
            'vhost' => Config::RABBITMQ_VHOST,
        ]);
        $this->connection->connect();

        $this->consChannel = new \AMQPChannel($this->connection);
        $this->consQueue = new \AMQPQueue($this->consChannel);
        $this->consQueue->setName(self::QUEUE);
        $this->consQueue->setFlags(AMQP_DURABLE);
        $this->consQueue->declareQueue();
        $this->consQueue->bind(self::EXCHANGE, self::QUEUE);
        $this->consChannel->setPrefetchCount(Config::PREFETCH_COUNT);
    }

    public function tearDown(): void
    {
        try {
            if ($this->connection !== null && $this->connection->isConnected()) {
                $this->connection->disconnect();
            }
        } catch (\Throwable) {
            // Connection may already be closed; safe to ignore.
        }
        $this->connection = null;
        $this->pubChannel = null;
        $this->consChannel = null;
        $this->pubExchange = null;
        $this->consQueue = null;
    }
}
