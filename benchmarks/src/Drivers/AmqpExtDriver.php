<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\Config;
use Bench\ScenarioMode;
use RuntimeException;

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
            throw new \RuntimeException('The pecl "amqp" extension is not loaded');
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
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->pubExchange === null || $this->pubChannel === null) {
            throw new RuntimeException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK) {
            for ($i = 0; $i < $count; $i++) {
                $ts = hrtime(true);
                $attrs = [
                    'message_id' => $this->uuid(),
                    'delivery_mode' => AMQP_DURABLE,
                ];
                $this->pubExchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_NOPARAM, $attrs);
            }
            return;
        }

        if (!$this->confirmMode) {
            $this->pubChannel->confirmSelect();
            $this->pubChannel->setConfirmCallback(
                function (): bool { return false; },
                function (): bool { return false; },
            );
            $this->confirmMode = true;
        }

        $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;

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
                }
            }
        }

        if ($count % $batchSize !== 0) {
            try {
                $this->pubChannel->waitForConfirm(5);
            } catch (\Throwable) {
            }
        }
    }

    public function consumeMessages(int $count): void
    {
        if ($this->consQueue === null) {
            throw new RuntimeException('Driver not set up');
        }

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;

        $consumed = 0;
        $consecutiveNulls = 0;
        while ($consumed < $count) {
            $flags = $autoAck ? AMQP_AUTOACK : AMQP_NOPARAM;
            $envelope = $this->consQueue->get($flags);
            if (!$envelope) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 3) {
                    break;
                }
                usleep(1000);
                continue;
            }
            $consecutiveNulls = 0;

            $body = $envelope->getBody();
            $this->recordReceived($envelope->getMessageId() ?? '');
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }

            if (!$autoAck) {
                $this->consQueue->ack($envelope->getDeliveryTag());
            }
            $consumed++;
        }
    }

    public function tearDown(): void
    {
        try {
            if ($this->connection !== null && $this->connection->isConnected()) {
                $this->connection->disconnect();
            }
        } catch (\Throwable) {
        }
        $this->connection = null;
        $this->pubChannel = null;
        $this->consChannel = null;
        $this->pubExchange = null;
        $this->consQueue = null;
    }
}
