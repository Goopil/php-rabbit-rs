<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Horizon;

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob as BaseRabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue as BaseRabbitMqQueue;
use Illuminate\Contracts\Events\Dispatcher;
use Laravel\Horizon\Events\JobDeleted;
use Laravel\Horizon\Events\JobPending;
use Laravel\Horizon\Events\JobPushed;
use Laravel\Horizon\Events\JobReserved;
use Laravel\Horizon\JobPayload;

class RabbitMqQueue extends BaseRabbitMqQueue
{
    protected mixed $lastPushed = null;

    public function push($job, $data = '', $queue = null)
    {
        $this->lastPushed = $job;

        $payload = $this->createPayload($job, $this->queueName($queue), $data);

        return $this->pushRaw($payload, $queue);
    }

    public function pushRaw($payload, $queue = null, array $options = [])
    {
        $payload = (new JobPayload($payload))->prepare($this->lastPushed ?? null)->value;

        $this->event($this->queueName($queue), new JobPending($payload));

        return tap(parent::pushRaw($payload, $queue, $options), function (string $messageId) use ($queue, $payload): void {
            $this->event($this->queueName($queue), new JobPushed($payload));
        });
    }

    public function later($delay, $job, $data = '', $queue = null)
    {
        $payload = (new JobPayload($this->createPayload($job, $this->queueName($queue), $data)))->prepare($job)->value;

        $this->event($this->queueName($queue), new JobPending($payload));

        return tap(parent::laterRawFromPayload($delay, $payload, $queue), function () use ($queue, $payload): void {
            $this->event($this->queueName($queue), new JobPushed($payload));
        });
    }

    public function pop($queue = null, $index = 0)
    {
        return tap(parent::pop($queue, $index), function (mixed $result) use ($queue): void {
            if ($result instanceof BaseRabbitMqJob) {
                $this->event($this->queueName($queue), new JobReserved($result->getRawBody()));
            }
        });
    }

    public function marshalJob(Delivery $delivery, $queue = null): BaseRabbitMqJob
    {
        return new RabbitMqJob(
            $this->container,
            $delivery,
            $this->connectionName,
            $this->queueName($queue),
            $this,
        );
    }

    public function deleteReserved(string $queue, BaseRabbitMqJob $job): void
    {
        $this->event($queue, new JobDeleted($job, $job->getRawBody()));
    }

    protected function event(string $queue, object $event): void
    {
        if ($this->container && $this->container->bound(Dispatcher::class)) {
            $this->container->make(Dispatcher::class)->dispatch(
                $event->connection($this->connectionName)->queue($queue)
            );
        }
    }
}
