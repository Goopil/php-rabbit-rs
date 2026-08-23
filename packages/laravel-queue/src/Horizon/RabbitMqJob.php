<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Horizon;

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob as BaseRabbitMqJob;
use Illuminate\Container\Container;

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
}
