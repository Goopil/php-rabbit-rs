<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;

class PhpAmqpExtFireAndForgetBenchmark extends AbstractBenchmark
{
    protected $connection;
    protected $channel;
    protected $exchange;
    protected $queue;
    protected $confirmMode = false;
    protected $pendingConfirms = 0;

    public function getName(): string
    {
        return 'php-amqp (Fire & Forget)';
    }

    public function setUp()
    {
        $this->connection = new \AMQPConnection([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'vhost' => Config::RABBITMQ_VHOST,
            'login' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD,
        ]);

        if (!$this->connection->isConnected()) {
            $this->connection->connect();
        }

        $this->channel = new \AMQPChannel($this->connection);
        $this->channel->qos(0, Config::PREFETCH_COUNT);

        $this->exchange = new \AMQPExchange($this->channel);
        $this->exchange->setName(Config::EXCHANGE_NAME);
        $this->exchange->setType(Config::EXCHANGE_TYPE);
        $this->exchange->setFlags($this->durableFlag());
        $this->exchange->declareExchange();

        $this->pendingConfirms = 0;
        $this->confirmMode = false;

        $this->queue = new \AMQPQueue($this->channel);
        $this->queue->setName(Config::QUEUE_NAME);
        $this->queue->setFlags($this->durableFlag());
        $this->queue->declareQueue();
        $this->queue->bind(Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        $this->queue->purge();
    }

    public function tearDown()
    {
        if ($this->connection && $this->connection->isConnected()) {
            $this->connection->disconnect();
        }
    }

    public function publishMessages(int $count)
    {
        $attributes = [
            'content_type' => 'application/json',
            'delivery_mode' => 2,
        ];

        for ($i = 0; $i < $count; $i++) {
            $this->exchange->publish(
                $this->createMessage("Message #$i"),
                Config::ROUTING_KEY,
                $this->noParamFlag(),
                $attributes
            );
            if ($this->confirmMode) {
                $this->pendingConfirms++;
                if (($i + 1) % Config::CONFIRM_CHUNK_SIZE === 0) {
                    $this->waitForAllConfirms();
                }
            }
        }

        $this->waitForAllConfirms();
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;

        $this->queue->consume(function (\AMQPEnvelope $envelope, \AMQPQueue $queue) use (&$consumed, $count) {
            $consumed++;
            $queue->ack($envelope->getDeliveryTag());
            if ($consumed >= $count) {
                return false;
            }
            return true;
        }, $this->noParamFlag());
    }

    protected function autoAckFlag(): int
    {
        return \defined('AMQP_AUTOACK') ? \AMQP_AUTOACK : 2;
    }

    protected function noParamFlag(): int
    {
        return \defined('AMQP_NOPARAM') ? \AMQP_NOPARAM : 0;
    }

    private function durableFlag(): int
    {
        return \defined('AMQP_DURABLE') ? \AMQP_DURABLE : 2;
    }

    protected function registerConfirmHandlers(): void
    {
        if (!method_exists($this->channel, 'setConfirmCallback')) {
            return;
        }

        $this->channel->setConfirmCallback(
            function (int $deliveryTag, bool $multiple): bool {
                if ($multiple) {
                    $this->pendingConfirms = 0;
                } else {
                    $this->pendingConfirms = max(0, $this->pendingConfirms - 1);
                }
                return true;
            },
            function (int $deliveryTag, bool $multiple, bool $requeue): bool {
                throw new \RuntimeException(sprintf(
                    'php-amqp publish was NACKed (tag=%s, multiple=%s, requeue=%s).',
                    $deliveryTag,
                    $multiple ? 'yes' : 'no',
                    $requeue ? 'yes' : 'no'
                ));
            }
        );

        $this->confirmMode = true;
    }

    protected function waitForAllConfirms(): void
    {
        if (!$this->confirmMode || !method_exists($this->channel, 'waitForConfirm')) {
            return;
        }

        $deadline = microtime(true) + Config::CONFIRM_WAIT_TIMEOUT;

        while ($this->pendingConfirms > 0) {
            $remaining = $deadline - microtime(true);
            if ($remaining <= 0) {
                throw new \RuntimeException('Timed out waiting for php-amqp publisher confirms.');
            }

            $interval = max(0.1, min(1.0, $remaining));

            if (method_exists($this->channel, 'waitForConfirmOrDie')) {
                $this->channel->waitForConfirmOrDie($interval);
            } else {
                $result = $this->channel->waitForConfirm($interval);
                if ($result === false) {
                    throw new \RuntimeException('php-amqp publish batch was NACKed.');
                }
            }
        }
    }
}
