<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\BenchmarkException;
use Bench\Config;
use Bench\ScenarioMode;
use Bunny\Channel;
use Bunny\Client;

class BunnyDriver extends AbstractBenchmark
{
    private const EXCHANGE = 'bench.bunny';
    private const QUEUE = 'bench.bunny';

    private ?Client $client = null;
    private ?Channel $channel = null;
    private bool $confirmMode = false;

    public function getName(): string
    {
        return 'bunny';
    }

    public function setUp(): void
    {
        $this->client = new Client([
            'host' => Config::RABBITMQ_HOST,
            'port' => Config::RABBITMQ_PORT,
            'user' => Config::RABBITMQ_USER,
            'password' => Config::RABBITMQ_PASSWORD,
            'vhost' => Config::RABBITMQ_VHOST,
        ]);
        $this->client->connect();
        $this->channel = $this->client->channel();

        $this->channel->exchangeDeclare(self::EXCHANGE, 'direct', false, true, false);
        $this->channel->queueDeclare(self::QUEUE, false, true, false, false);
        $this->channel->queueBind(self::QUEUE, self::EXCHANGE, self::QUEUE);
        $this->channel->qos(0, Config::PREFETCH_COUNT);
        $this->channel->queuePurge(self::QUEUE);
    }

    public function purgeQueue(): void
    {
        if ($this->channel !== null) {
            try {
                $this->channel->queuePurge(self::QUEUE);
            } catch (\Throwable) {
                // Queue may not exist yet; safe to ignore.
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->channel === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK
            || $this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
            for ($i = 0; $i < $count; $i++) {
                $ts = hrtime(true);
                $this->channel->publish(
                    pack('P', $ts) . $this->createMessage((string) $i),
                    ['delivery-mode' => 2, 'message-id' => $this->uuid()],
                    self::EXCHANGE,
                    self::QUEUE,
                    false,
                );
            }
            return;
        }

        $batchSize = $this->scenarioMode === ScenarioMode::LARAVEL_DISPATCH ? 64 : 256;
        if (!$this->confirmMode) {
            $this->channel->confirmSelect();
            $this->confirmMode = true;
        }

        $pending = 0;

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $this->channel->publish(
                pack('P', $ts) . $this->createMessage((string) $i),
                ['delivery-mode' => 2, 'message-id' => $this->uuid()],
                self::EXCHANGE,
                self::QUEUE,
                true,
            );
            $this->publishSeq++;
            $pending++;

            if ($pending >= $batchSize) {
                $this->waitForConfirms();
                $pending = 0;
            }
        }

        if ($pending > 0) {
            $this->waitForConfirms();
        }
    }

    private int $publishSeq = 0;

    private function waitForConfirms(): void
    {
        $targetSeq = $this->publishSeq;
        $listener = function ($frame) use ($targetSeq) {
            if ($frame->deliveryTag >= $targetSeq) {
                $this->client->stop();
            }
        };
        $this->channel->addAckListener($listener);
        $this->client->run(10);
        $this->client->stop();
        $this->channel->removeAckListener($listener);
    }

    public function consumeMessages(int $count): void
    {
        if ($this->channel === null) {
            throw new BenchmarkException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::LARAVEL_WORKER) {
            $this->consumeSingleGet($count);
            return;
        }

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;

        $consumerTag = 'bench_bunny_consumer';

        $consumed = 0;
        $callback = $this->makeConsumeCallback($count, $autoAck, $consumerTag, $consumed);

        $this->channel->consume($callback, self::QUEUE, $consumerTag, false, $autoAck);

        $this->runConsumerLoop($count, $consumed);
    }

    private function makeConsumeCallback(int $count, bool $autoAck, string $consumerTag, int &$consumed): \Closure
    {
        $channel = $this->channel;
        $client = $this->client;

        return function ($message) use ($count, &$consumed, $autoAck, $channel, $consumerTag, $client): void {
            $body = $message->content;
            $this->recordReceived($message->getHeader('message-id', ''));
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $consumed++;
            if (!$autoAck) {
                $channel->ack($message);
            }
            if ($consumed >= $count) {
                $channel->cancel($consumerTag);
                $client->stop();
            }
        };
    }

    private function consumeSingleGet(int $count): void
    {
        $consumed = 0;
        $consecutiveEmpty = 0;
        while ($consumed < $count) {
            $message = $this->channel->get(self::QUEUE, false);
            if ($message === null) {
                $consecutiveEmpty++;
                if ($consecutiveEmpty >= 3) {
                    break;
                }
                usleep(100_000);
                continue;
            }
            $consecutiveEmpty = 0;

            $body = $message->content;
            $this->recordReceived($message->getHeader('message-id', ''));
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $this->channel->ack($message);
            $consumed++;
        }
    }

    private function runConsumerLoop(int $count, int &$consumed): void
    {
        $consecutiveTimeouts = 0;

        while ($consumed < $count) {
            try {
                $this->client->run(1);
                $consecutiveTimeouts = 0;
            } catch (\Bunny\Exception\ClientException) {
                $consecutiveTimeouts++;
                if ($consecutiveTimeouts >= 3) {
                    break;
                }
            } catch (\Throwable) {
                break;
            }
        }
    }

    public function tearDown(): void
    {
        if ($this->client !== null) {
            try {
                $this->client->disconnect();
            } catch (\Throwable) {
                // Connection may already be closed; safe to ignore.
            }
            $this->client = null;
        }
        $this->channel = null;
    }
}
