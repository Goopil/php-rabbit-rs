<?php

namespace Goopil\RabbitRs\Benchmarks;

use Goopil\RabbitRs\Benchmarks\Config;
use Goopil\RabbitRs\PhpClient;
use Goopil\RabbitRs\AmqpMessage;
use Goopil\RabbitRs\PhpChannel;

class RabbitRsBatchConfirmBenchmark extends AbstractBenchmark
{
    protected $client;
    protected $channel;
    protected $messageTemplate;

    public function getName(): string
    {
        return 'RabbitRs (Batch Confirm)';
    }

    public function setUp()
    {
        $this->client = new PhpClient([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'user' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD,
            'vhost' => Config::RABBITMQ_VHOST
        ]);

        $this->client->connect();
        $this->channel = $this->client->openChannel();
        $this->channel->confirmSelect();

        // Use non-durable for better performance in benchmarks
        $this->channel->exchangeDeclare(Config::EXCHANGE_NAME, 'direct', [
            'durable' => true
        ]);
        $this->channel->queueDeclare(Config::QUEUE_NAME, [
            'durable' => true,
        ]);
        $this->channel->queueBind(Config::QUEUE_NAME, Config::EXCHANGE_NAME, Config::ROUTING_KEY);
        $this->channel->queuePurge(Config::QUEUE_NAME);

        // Pre-create a message template for better performance
        $messageBody = json_encode([
            'id' => 'benchmark',
            'timestamp' => microtime(true),
            'data' => str_repeat('x', 100) // Fixed size payload
        ]);

        $this->messageTemplate = new AmqpMessage($messageBody, [
            'content_type' => 'application/json'
        ]);
    }

    public function tearDown()
    {
        if ($this->channel) {
            try {
                $this->channel->close();
            } catch (Exception $e) {
                // Ignore errors during teardown
            }
        }
        if ($this->client) {
            try {
                $this->client->close();
            } catch (Exception $e) {
                // Ignore errors during teardown
            }
        }
    }

    public function publishMessages(int $count)
    {
        // Reuse the same message template for all publishes to minimize object creation overhead
        for ($i = 0; $i < $count; $i++) {
            $this->channel->basicPublish(
                Config::EXCHANGE_NAME,
                Config::ROUTING_KEY,
                $this->messageTemplate
            );
        }

        // Wait for all messages in the batch to be confirmed.
        $this->channel->waitForConfirms();
    }

    public function consumeMessages(int $count)
    {
        $consumed = 0;
        $callback = function ($delivery) use (&$consumed, $count) {
            $consumed++;
            $delivery->ack();
            if ($consumed >= $count) {
                // Cancel consumer after reaching target count
                $this->channel->basicCancel();
            }
        };

        // Set QoS for better consume performance
        $this->channel->qos(min($count, 1000), ['global' => false]);

        // Start consuming on the same channel
        $this->channel->simpleConsume(Config::QUEUE_NAME, $callback, [
            'no_ack' => false
        ]);

        // Wait until we've consumed all messages
        while ($consumed < $count) {
            $this->channel->wait(100, min($count - $consumed, 100));
        }
    }
}
