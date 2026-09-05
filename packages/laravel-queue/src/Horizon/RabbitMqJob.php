<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Horizon;

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob as BaseRabbitMqJob;
use Illuminate\Container\Container;
use Illuminate\Contracts\Events\Dispatcher;
use Illuminate\Queue\ManuallyFailedException;
use Laravel\Horizon\Events\JobFailed;

class RabbitMqJob extends BaseRabbitMqJob
{
    public function __construct(
        Container $container,
        Delivery $delivery,
        string $connectionName,
        string $queue,
        private readonly RabbitMqQueue $rabbitmq,
    ) {
        parent::__construct($container, $delivery, $connectionName, $queue);
    }

    public function delete(): void
    {
        if ($this->isDeletedOrReleased()) {
            return;
        }

        parent::delete();

        $this->rabbitmq->deleteReserved($this->queue, $this);
    }

    /**
     * Horizon bridges the framework's JobFailed event to its own through the
     * MarshalFailedEvent listener, which only recognizes Illuminate\Jobs\RedisJob.
     * Dispatch Horizon's event here so exhausted rabbit-rs jobs are recorded as
     * failed by Horizon instead of remaining marked completed by JobDeleted.
     */
    public function fail($e = null): void
    {
        parent::fail($e);

        if (! $this->container->bound(Dispatcher::class)) {
            return;
        }

        $this->container->make(Dispatcher::class)->dispatch(
            (new JobFailed($e ?: new ManuallyFailedException, $this, $this->getRawBody()))
                ->connection($this->connectionName)
                ->queue($this->queue)
        );
    }
}
