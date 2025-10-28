<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;
use Bunny\Client;
use Bunny\Message;
use Bunny\Channel;

class BunnyBatchConfirmBenchmark extends AbstractBenchmark
{
    protected $client;
    protected $channel;

    public function getName(): string
    {
        return 'Bunny (Batch Confirm)';
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
        $this->channel->confirmSelect();

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
        $promises = [];
        for ($i = 0; $i < $count; $i++) {
            $messageBody = $this->createMessage("Message #$i");
            $promises[] = $this->channel->publish($messageBody, [
                'content-type' => 'application/json'
            ], Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        }

        $allPromise = \React\Promise\all($promises);

        $resolved = false;
        $exception = null;

        $allPromise->then(
            function ($value) use (&$resolved) {
                $resolved = true;
            },
            function ($reason) use (&$resolved, &$exception) {
                $resolved = true;
                $exception = $reason;
            }
        );

        // In Bunny v0.5, there's no blocking `await` call.
        // We must manually run the event loop until the promise is settled.
        while (!$resolved) {
            $this->client->run(0.01); // Process I/O for 10ms
        }

        if ($exception) {
            throw $exception instanceof \Exception ? $exception : new \Exception('Bunny publish batch was NACKed.');
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
