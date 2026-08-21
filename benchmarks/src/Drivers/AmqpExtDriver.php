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
    private $channel = null;
    private $exchange = null;
    private $queue = null;

    public function __construct()
    {
        if (!extension_loaded('amqp')) {
            throw new DriverUnavailableException('The pecl "amqp" extension is not loaded');
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

    public function publishMessages(int $count): void
    {
        if ($this->exchange === null || $this->channel === null) {
            throw new RuntimeException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET) {
            for ($i = 0; $i < $count; $i++) {
                $ts = hrtime(true);
                $attrs = [
                    'message_id' => $this->uuid(),
                    'delivery_mode' => AMQP_DURABLE,
                ];
                $this->exchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_NOPARAM, $attrs);
            }
            return;
        }

        $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
        $this->channel->confirmSelect();

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $attrs = [
                'message_id' => $this->uuid(),
                'delivery_mode' => AMQP_DURABLE,
            ];
            $this->exchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_MANDATORY, $attrs);

            if (($i + 1) % $batchSize === 0) {
                $this->channel->waitForConfirms(5);
            }
        }

        $this->channel->waitForConfirms(5);
    }

    public function consumeMessages(int $count): void
    {
        if ($this->queue === null) {
            throw new RuntimeException('Driver not set up');
        }

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;

        $consumed = 0;
        $consecutiveNulls = 0;
        while ($consumed < $count) {
            $flags = $autoAck ? AMQP_AUTOACK : AMQP_NOPARAM;
            $envelope = $this->queue->get($flags);
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

            if (!$autoAck) {
                $this->queue->ack($envelope->getDeliveryTag());
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
        $this->channel = null;
        $this->exchange = null;
        $this->queue = null;
    }

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
