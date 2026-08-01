<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Queue\Queue as QueueContract;
use Illuminate\Queue\Queue;
use LogicException;

final class RabbitMqQueue extends Queue implements QueueContract
{
    /**
     * @param array<string, array<string, mixed>> $routes
     */
    public function __construct(
        private readonly Pool $pool,
        private readonly array $routes,
        private readonly string $defaultQueue,
    ) {}

    public function size($queue = null)
    {
        throw self::operationsPending();
    }

    public function pendingSize($queue = null)
    {
        throw self::operationsPending();
    }

    public function delayedSize($queue = null)
    {
        throw self::operationsPending();
    }

    public function reservedSize($queue = null)
    {
        throw self::operationsPending();
    }

    public function creationTimeOfOldestPendingJob($queue = null)
    {
        throw self::operationsPending();
    }

    public function push($job, $data = '', $queue = null)
    {
        throw self::operationsPending();
    }

    public function pushRaw($payload, $queue = null, array $options = [])
    {
        throw self::operationsPending();
    }

    public function later($delay, $job, $data = '', $queue = null)
    {
        throw self::operationsPending();
    }

    public function pop($queue = null)
    {
        throw self::operationsPending();
    }

    private static function operationsPending(): LogicException
    {
        return new LogicException('Rabbit MQ queue operations are not implemented until Task 18.');
    }
}