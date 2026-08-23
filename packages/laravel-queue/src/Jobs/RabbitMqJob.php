<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Jobs;

use Goopil\RabbitRs\Delivery;
use Illuminate\Container\Container;
use Illuminate\Contracts\Queue\Job as JobContract;
use Illuminate\Queue\Jobs\Job;
use InvalidArgumentException;

class RabbitMqJob extends Job implements JobContract
{
    private ?Delivery $delivery;

    private readonly string $rawBody;

    private readonly string $jobId;

    private readonly int $deliveryAttempts;

    public function __construct(
        Container $container,
        Delivery $delivery,
        string $connectionName,
        string $queue,
    ) {
        $metadata = $delivery->metadata();

        $messageId = $metadata['message_id'] ?? null;
        if (!is_string($messageId) || $messageId === '') {
            throw new InvalidArgumentException(
                "Delivery is missing required 'message_id' metadata — cannot create job"
            );
        }

        $rawBody = $delivery->payload();
        json_decode($rawBody, true);
        if (json_last_error() !== JSON_ERROR_NONE) {
            throw new InvalidArgumentException(
                "Delivery payload is not valid JSON: " . json_last_error_msg()
            );
        }

        $this->container = $container;
        $this->delivery = $delivery;
        $this->connectionName = $connectionName;
        $this->queue = $queue;
        $this->rawBody = $rawBody;
        $this->jobId = $messageId;
        $this->deliveryAttempts = (int) ($metadata['attempts'] ?? 0);
    }

    public function getJobId(): string
    {
        return $this->jobId;
    }

    public function getRawBody(): string
    {
        return $this->rawBody;
    }

    public function attempts(): int
    {
        return $this->deliveryAttempts;
    }

    public function delete(): void
    {
        if ($this->isDeletedOrReleased() || $this->delivery === null) {
            return;
        }

        $this->delivery->ack();
        $this->delivery = null;
        parent::delete();
    }

    public function release($delay = 0): void
    {
        if ($this->isDeletedOrReleased() || $this->delivery === null) {
            return;
        }

        $seconds = max(0, $this->secondsUntil($delay));
        if ($seconds > intdiv(PHP_INT_MAX, 1000)) {
            throw new InvalidArgumentException('release delay exceeds the supported millisecond range');
        }

        $this->delivery->release($seconds * 1000);
        $this->delivery = null;
        parent::release($delay);
    }
}