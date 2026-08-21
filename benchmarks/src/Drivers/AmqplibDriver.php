<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\Config;
use Bench\ScenarioMode;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;
use RuntimeException;

class AmqplibDriver extends AbstractBenchmark
{
    private const EXCHANGE = 'bench.amqplib';
    private const QUEUE = 'bench.amqplib';

    private ?AMQPStreamConnection $pubConnection = null;
    private ?AMQPStreamConnection $consConnection = null;
    private $pubChannel = null;
    private $consChannel = null;

    public function getName(): string
    {
        return 'amqplib';
    }

    public function setUp(): void
    {
        $this->pubConnection = new AMQPStreamConnection(
            Config::RABBITMQ_HOST,
            Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER,
            Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST,
        );
        $this->consConnection = new AMQPStreamConnection(
            Config::RABBITMQ_HOST,
            Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER,
            Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST,
        );
        $this->pubChannel = $this->pubConnection->channel();
        $this->consChannel = $this->consConnection->channel();
        $this->pubChannel->exchange_declare(self::EXCHANGE, 'direct', false, true, false);
        $this->pubChannel->queue_declare(self::QUEUE, false, true, false, false);
        $this->pubChannel->queue_bind(self::QUEUE, self::EXCHANGE, self::QUEUE);
        $this->consChannel->basic_qos(0, 16, false);
    }

    public function publishMessages(int $count): void
    {
        if ($this->pubChannel === null) {
            throw new RuntimeException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET) {
            for ($i = 0; $i < $count; $i++) {
                $ts = hrtime(true);
                $msg = new AMQPMessage(pack('P', $ts) . $this->createMessage((string) $i), [
                    'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                    'message_id' => $this->uuid(),
                ]);
                $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, false);
            }
            return;
        }

        $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
        $this->pubChannel->confirm_select();

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $msg = new AMQPMessage(pack('P', $ts) . $this->createMessage((string) $i), [
                'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                'message_id' => $this->uuid(),
            ]);
            $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, true);

            if ($batchSize > 1 && ($i + 1) % $batchSize === 0) {
                $this->pubChannel->wait_for_pending_acks(5);
            }
        }

        $this->pubChannel->wait_for_pending_acks(5);
    }

    public function consumeMessages(int $count): void
    {
        if ($this->consChannel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $consumed = 0;
        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;
        $batchAckSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 100 : 1;

        $callback = function (AMQPMessage $msg) use ($count, &$consumed, $autoAck, $batchAckSize): void {
            $body = $msg->getBody();
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $consumed++;
            if (!$autoAck) {
                $msg->ack();
            }
        };

        $noAck = $autoAck;
        $this->consChannel->basic_consume(self::QUEUE, '', false, $noAck, false, false, $callback);

        $consecutiveTimeouts = 0;
        while ($consumed < $count) {
            try {
                $this->consChannel->wait(null, false, 1);
                $consecutiveTimeouts = 0;
            } catch (\PhpAmqpLib\Exception\AMQPTimeoutException) {
                $consecutiveTimeouts++;
                if ($consecutiveTimeouts >= 3) {
                    break;
                }
            }
        }
    }

    public function tearDown(): void
    {
        try {
            if ($this->pubChannel !== null) {
                $this->pubChannel->close();
            }
        } catch (\Throwable) {
        }
        try {
            if ($this->consChannel !== null) {
                $this->consChannel->close();
            }
        } catch (\Throwable) {
        }
        try {
            if ($this->pubConnection !== null) {
                $this->pubConnection->close();
            }
        } catch (\Throwable) {
        }
        try {
            if ($this->consConnection !== null) {
                $this->consConnection->close();
            }
        } catch (\Throwable) {
        }
        $this->pubChannel = null;
        $this->consChannel = null;
        $this->pubConnection = null;
        $this->consConnection = null;
    }

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
