<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;

class AmqplibFireAndForgetBenchmark extends AbstractBenchmark
{
    protected $connection;
    protected $channel;

    public function getName(): string
    {
        return 'AMQPLib (Fire & Forget)';
    }

    public function setUp()
    {
        $this->connection = new AMQPStreamConnection(
            Config::RABBITMQ_HOST,
            Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER,
            Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST
        );

        $this->channel = $this->connection->channel();
        $this->channel->exchange_declare(Config::EXCHANGE_NAME, 'direct', false, true, false);
        $this->channel->queue_declare(Config::QUEUE_NAME, false, true, false, false);
        $this->channel->queue_bind(Config::QUEUE_NAME, Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        $this->channel->queue_purge(Config::QUEUE_NAME);
    }

    public function tearDown()
    {
        if ($this->channel) {
            $this->channel->close();
        }
        if ($this->connection) {
            $this->connection->close();
        }
    }

    public function publishMessages(int $count)
    {
        for ($i = 0; $i < $count; $i++) {
            $messageBody = $this->createMessage("Message #$i");
            $msg = new AMQPMessage($messageBody, [
                'content_type' => 'application/json',
                'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT
            ]);
            $this->channel->basic_publish($msg, Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        }
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;
        $callback = function ($msg) use (&$consumed, $count) {
            $consumed++;
            $msg->ack();
            if ($consumed >= $count) {
                $this->channel->basic_cancel($msg->getConsumerTag());
            }
        };

        $this->channel->basic_consume(Config::QUEUE_NAME, '', false, false, false, false, $callback);

        while ($consumed < $count) {
            $this->channel->wait(null, false, 10);
        }
    }
}
