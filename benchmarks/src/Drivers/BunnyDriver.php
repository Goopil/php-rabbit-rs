<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\Config;
use Bench\ScenarioMode;
use Bunny\Channel;
use Bunny\Client;
use RuntimeException;

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
            }
        }
    }

    public function publishMessages(int $count): void
    {
        if ($this->channel === null) {
            throw new RuntimeException('Driver not set up');
        }

        if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK) {
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

        $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
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
                $this->waitForConfirms($pending);
                $pending = 0;
            }
        }

        if ($pending > 0) {
            $this->waitForConfirms($pending);
        }
    }

    private int $publishSeq = 0;

    private function waitForConfirms(int $expected): void
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
            throw new RuntimeException('Driver not set up');
        }

        $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
            || $this->scenarioMode === ScenarioMode::AUTO_ACK;
        $consumed = 0;
        $consecutiveNulls = 0;

        while ($consumed < $count) {
            $message = $this->channel->get(self::QUEUE, $autoAck);
            if ($message === null) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 3) {
                    break;
                }
                usleep(1000);
                continue;
            }
            $consecutiveNulls = 0;

            $body = $message->content;
            $this->recordReceived($message->getHeader('message-id', ''));
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }

            if (!$autoAck) {
                $this->channel->ack($message);
            }
            $consumed++;
        }
    }

    public function tearDown(): void
    {
        if ($this->client !== null) {
            try {
                $this->client->disconnect();
            } catch (\Throwable) {
            }
            $this->client = null;
        }
        $this->channel = null;
    }
}
