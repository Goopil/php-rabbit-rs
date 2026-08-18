<?php

declare(strict_types=1);

namespace Bench\Drivers;

use Bench\Metrics;
use PhpAmqpLib\Connection\AMQPStreamConnection;
use PhpAmqpLib\Message\AMQPMessage;
use RuntimeException;

class PhpAmqplibDriver implements Driver
{
    use Metrics;

    private const EXCHANGE = 'bench.amqplib';
    private const QUEUE = 'bench.amqplib';

    private ?AMQPStreamConnection $pubConnection = null;
    private ?AMQPStreamConnection $consConnection = null;
    private $pubChannel = null;
    private $consChannel = null;
    private int $publishCount = 0;
    private int $consumeCount = 0;
    private int $losses = 0;
    private int $returns = 0;

    public function setup(): void
    {
        $this->pubConnection = new AMQPStreamConnection('127.0.0.1', 5672, 'admin', 'admin_lab', '/');
        $this->consConnection = new AMQPStreamConnection('127.0.0.1', 5672, 'admin', 'admin_lab', '/');
        $this->pubChannel = $this->pubConnection->channel();
        $this->consChannel = $this->consConnection->channel();
        $this->pubChannel->exchange_declare(self::EXCHANGE, 'direct', false, true, false);
        $this->pubChannel->queue_declare(self::QUEUE, false, true, false, false);
        $this->pubChannel->queue_bind(self::QUEUE, self::EXCHANGE, self::QUEUE);
        $this->consChannel->basic_qos(0, 16, false);
    }

    public function publish(array $messages, string $safety = 'safest'): void
    {
        if ($this->pubChannel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $mandatory = ($safety === 'safest');
        $useConfirms = ($safety !== 'unsafe');

        if ($useConfirms) {
            $this->pubChannel->confirm_select();
        }

        $this->returns = 0;
        if ($mandatory) {
            $this->pubChannel->set_return_listener(function () {
                $this->returns++;
            });
        }

        foreach ($messages as $payload) {
            $ts = hrtime(true);
            $msg = new AMQPMessage(pack('P', $ts) . $payload, [
                'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                'message_id' => $this->uuid(),
            ]);
            $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, $mandatory);
        }

        if ($useConfirms) {
            try {
                $this->pubChannel->wait_for_pending_acks(5);
            } catch (\PhpAmqpLib\Exception\AMQPRuntimeException $e) {
                if (str_contains($e->getMessage(), 'unknown delivery_tag')) {
                    $this->losses = 1;
                } else {
                    throw $e;
                }
            }
        }

        if ($mandatory) {
            $this->losses = $this->returns;
        }

        $this->publishCount += count($messages);
    }

    public function consume(int $count): void
    {
        if ($this->consChannel === null) {
            throw new RuntimeException('Driver not set up');
        }

        $this->consumeCount = 0;
        $this->losses = 0;

        $callback = function (AMQPMessage $msg) use ($count): void {
            $body = $msg->getBody();
            if (strlen($body) >= 8) {
                $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
                if ($ts !== null) {
                    $elapsedNs = hrtime(true) - (int) $ts;
                    $this->recordLatency($elapsedNs / 1_000_000);
                }
            }
            $msg->ack();
            $this->consumeCount++;
        };

        $this->consChannel->basic_consume(self::QUEUE, '', false, false, false, false, $callback);

        $consecutiveTimeouts = 0;
        while ($this->consumeCount < $count) {
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

        $this->losses = max(0, $count - $this->consumeCount);
    }

    public function reset(): void
    {
        if ($this->pubChannel !== null) {
            try {
                $this->pubChannel->queue_purge(self::QUEUE);
            } catch (\Throwable) {
            }
        }
        $this->resetLatencies();
    }

    public function teardown(): void
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

    public function metrics(): array
    {
        $elapsed = $this->elapsedSeconds();
        return $this->buildMetrics(
            $this->consumeCount > 0 ? $this->consumeCount : $this->publishCount,
            $elapsed,
            connections: 1,
            channels: 1,
            losses: $this->losses,
        );
    }

    public function name(): string
    {
        return 'php-amqplib';
    }

    private function uuid(): string
    {
        $bytes = random_bytes(16);
        $bytes[6] = chr((ord($bytes[6]) & 0x0f) | 0x40);
        $bytes[8] = chr((ord($bytes[8]) & 0x3f) | 0x80);
        return vsprintf('%s%s-%s-%s-%s-%s%s%s', str_split(bin2hex($bytes), 4));
    }
}
