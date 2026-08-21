<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\AbstractBenchmark;
use Bench\Config;
use Bunny\Channel;
use Bunny\Client;
use RuntimeException;

class BunnyDriver extends AbstractBenchmark
{
    private const EXCHANGE = 'bench.bunny';
    private const QUEUE = 'bench.bunny';

    private ?Client $client = null;
    private ?Channel $channel = null;

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
        $this->channel->queuePurge(self::QUEUE);
    }

    public function publishMessages(int $count): void
    {
        if ($this->channel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $this->channel->confirmSelect();

        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $this->channel->publish(
                pack('P', $ts) . $this->createMessage((string) $i),
                ['delivery-mode' => 2, 'message-id' => $this->uuid()],
                self::EXCHANGE,
                self::QUEUE,
                true,
            );
        }

        $this->channel->waitForConfirms();
    }

    public function consumeMessages(int $count): void
    {
        if ($this->channel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $consumed = 0;

        $this->channel->consume(function ($message, $channel) use ($count, &$consumed) {
            $body = $message->content;
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $channel->ack($message);
            $consumed++;
            if ($consumed >= $count) {
                $channel->cancel('');
            }
        }, self::QUEUE);

        $consecutiveTimeouts = 0;
        while ($consumed < $count) {
            try {
                $this->client->run(1);
                $consecutiveTimeouts = 0;
            } catch (\Throwable) {
                $consecutiveTimeouts++;
                if ($consecutiveTimeouts >= 3) {
                    break;
                }
            }
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

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
