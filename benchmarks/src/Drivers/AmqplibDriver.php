<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;
use Bench\Config;
use Bench\ScenarioMode;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;

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
            false,
            'AMQPLAIN',
            null,
            'en_US',
            10,
            60,
            null,
            false,
            0,
            60,
        );
        $this->consConnection = new AMQPStreamConnection(
            Config::RABBITMQ_HOST,
            Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER,
            Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST,
            false,
            'AMQPLAIN',
            null,
            'en_US',
            10,
            60,
            null,
            false,
            0,
            60,
        );
        $this->pubChannel = $this->pubConnection->channel();
        $this->consChannel = $this->consConnection->channel();
        $this->pubChannel->exchange_declare(self::EXCHANGE, 'direct', false, true, false);
        $this->pubChannel->queue_declare(self::QUEUE, false, true, false, false);
        $this->pubChannel->queue_bind(self::QUEUE, self::EXCHANGE, self::QUEUE);
        $this->consChannel->basic_qos(0, 16, false);
    }

    public function purgeQueue(): void
    {
        if ($this->pubChannel !== null) {
            try {
                $this->pubChannel->queue_purge(self::QUEUE);
            } catch (\Throwable) {
                // Queue may not exist yet; safe to ignore.
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->pubChannel === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK
            || $this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
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

        $batchSize = $this->scenarioMode === ScenarioMode::LARAVEL_DISPATCH ? 64 : 256;
        try {
            $this->pubChannel->close();
        } catch (\Throwable) {
            // Best-effort: ignore errors during cleanup/teardown.
        }
        $this->pubChannel = $this->pubConnection->channel();
        $this->pubChannel->confirm_select();

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $msg = new AMQPMessage(pack('P', $ts) . $this->createMessage((string) $i), [
                'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                'message_id' => $this->uuid(),
            ]);
            $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, true);

            if (($i + 1) % $batchSize === 0) {
                $this->waitForPublishConfirms();
            }
        }

        $this->waitForPublishConfirms();
    }

    private function waitForPublishConfirms(): void
    {
        if ($this->scenarioMode === ScenarioMode::LARAVEL_DISPATCH) {
            $this->pubChannel->wait_for_pending_acks_returns(5);
            return;
        }
        $this->pubChannel->wait_for_pending_acks(5);
    }

    public function consumeMessages(int $count): void
    {
        if ($this->consChannel === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
            $this->consumeSingleGet($count);
            return;
        }

        try {
            $this->consChannel->close();
        } catch (\Throwable) {
            // Best-effort: ignore errors during cleanup/teardown.
        }
        $this->consChannel = $this->consConnection->channel();
        $this->consChannel->basic_qos(0, Config::PREFETCH_COUNT, false);

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;
        $consumerTag = 'bench_consumer';

        $consumed = 0;
        $callback = $this->makeConsumeCallback($count, $autoAck, $consumerTag, $consumed);
        $this->consChannel->basic_consume(self::QUEUE, $consumerTag, false, $autoAck, false, false, $callback);

        $this->consumeWithTimeouts($count, $consumed);
    }

    private function consumeSingleGet(int $count): void
    {
        try {
            $this->consChannel->close();
        } catch (\Throwable) {
            // Best-effort: ignore errors during cleanup/teardown.
        }
        $this->consChannel = $this->consConnection->channel();
        $this->consChannel->basic_qos(0, Config::PREFETCH_LARAVEL, false);

        $consumed = 0;
        $consecutiveEmpty = 0;
        while ($consumed < $count) {
            $msg = $this->consChannel->basic_get(self::QUEUE, false);
            if ($msg === null) {
                $consecutiveEmpty++;
                if ($consecutiveEmpty >= 3) {
                    break;
                }
                usleep(100_000);
                continue;
            }
            $consecutiveEmpty = 0;

            $body = $msg->getBody();
            $this->recordReceived($msg->get('message_id'));
            $this->recordLatencyFromPayload($body);
            $msg->ack();
            $consumed++;
        }
    }

    private function makeConsumeCallback(int $count, bool $autoAck, string $consumerTag, int &$consumed): \Closure
    {
        return function (AMQPMessage $msg) use ($count, &$consumed, $autoAck, $consumerTag): void {
            $body = $msg->getBody();
            $this->recordReceived($msg->get('message_id'));
            $this->recordLatencyFromPayload($body);
            $consumed++;
            if (!$autoAck) {
                $msg->ack();
            }
            if ($consumed >= $count) {
                $msg->getChannel()->basic_cancel($consumerTag);
            }
        };
    }

    private function consumeWithTimeouts(int $count, int &$consumed): void
    {
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
            } catch (\PhpAmqpLib\Exception\AMQPProtocolChannelException) {
                break;
            } catch (\PhpAmqpLib\Exception\AMQPChannelClosedException) {
                break;
            } catch (\Throwable) {
                break;
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
            // Best-effort: ignore errors during cleanup/teardown.
        }
        try {
            if ($this->consChannel !== null) {
                $this->consChannel->close();
            }
        } catch (\Throwable) {
            // Best-effort: ignore errors during cleanup/teardown.
        }
        try {
            if ($this->pubConnection !== null) {
                $this->pubConnection->close();
            }
        } catch (\Throwable) {
            // Connection may already be closed; safe to ignore.
        }
        try {
            if ($this->consConnection !== null) {
                $this->consConnection->close();
            }
        } catch (\Throwable) {
            // Connection may already be closed; safe to ignore.
        }
        $this->pubChannel = null;
        $this->consChannel = null;
        $this->pubConnection = null;
        $this->consConnection = null;
    }
}
