<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;
use Bunny\Client;
use Bunny\Message;
use Bunny\Channel;

class BunnyFireAndForgetBenchmark extends AbstractBenchmark
{
    protected $client;
    protected $channel;

    public function getName(): string
    {
        return 'Bunny (Fire & Forget)';
    }

    public function setUp()
    {
        $this->client = new Client([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'vhost' => Config::RABBITMQ_VHOST,
            'user' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD
        ]);

        $this->client->connect();
        $this->channel = $this->client->channel();

        $this->channel->exchangeDeclare(Config::EXCHANGE_NAME, 'direct', false, true, false);
        $this->channel->queueDeclare(Config::QUEUE_NAME, false, true, false, false);
        $this->channel->queueBind(Config::QUEUE_NAME, Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        $this->channel->queuePurge(Config::QUEUE_NAME);
    }

    public function tearDown()
    {
        if ($this->channel) {
            $this->channel->close();
        }
        if ($this->client) {
            $this->client->disconnect();
        }
    }

    public function publishMessages(int $count)
    {
        for ($i = 0; $i < $count; $i++) {
            $messageBody = $this->createMessage("Message #$i");
            $this->channel->publish($messageBody, [
                'content-type' => 'application/json'
            ], Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        }
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;
        $callback = function (Message $msg, Channel $ch) use (&$consumed, $count) {
            $consumed++;
            $ch->ack($msg);
            if ($consumed >= $count) {
                $ch->cancel($msg->consumerTag);
            }
        };

        $this->channel->consume(function (Message $msg, Channel $ch) use (&$consumed, $count, $callback) {
            return $callback($msg, $ch);
        }, Config::QUEUE_NAME);

        while ($consumed < $count) {
            $this->client->run(0.1); // Run for 100ms intervals
        }
    }
}
