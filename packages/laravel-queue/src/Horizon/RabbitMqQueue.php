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
    public function push($job, $data = '', $queue = null)
    {
        $queueName = $this->queueName($queue);
        $payload = (new JobPayload($this->createPayload($job, $queueName, $data)))->prepare($job)->value;

        $this->event($queueName, new JobPending($payload));

        return $this->enqueueUsing(
            $job,
            $payload,
            $queue,
            null,
            fn (string $payload, ?string $queue): string => $this->publishHorizonPayload($payload, $queue),
        );
    }

    public function later($delay, $job, $data = '', $queue = null)
    {
        $queueName = $this->queueName($queue);
        $payload = (new JobPayload($this->createPayload($job, $queueName, $data, $delay)))->prepare($job)->value;

        $this->event($queueName, new JobPending($payload));

        return $this->enqueueUsing(
            $job,
            $payload,
            $queue,
            $delay,
            fn (string $payload, ?string $queue, mixed $delay): string => $this->publishHorizonPayload(
                $payload,
                $queue,
                $this->delayMilliseconds($delay),
            ),
        );
    }

    /**
     * Prepares Horizon-aware payloads for batched jobs and marks them as
     * pending in the dashboard before the native batch publication.
     *
     * @param  list<mixed>  $jobs
     * @return list<array{job: mixed, delay: mixed, payload: string, native: array<string, mixed>}>
     */
    protected function prepareBatch(array $jobs, mixed $data, mixed $queue): array
    {
        return array_map(function (array $prepared) use ($queue) {
            $payload = (new JobPayload($prepared['payload']))->prepare($prepared['job'])->value;
            $prepared['native']['payload'] = $payload;
            $this->event($this->queueName($queue), new JobPending($payload));

            return [...$prepared, 'payload' => $payload];
        }, parent::prepareBatch($jobs, $data, $queue));
    }

    /**
     * Publishes the native batch, then marks every job as pushed in the
     * dashboard once the publication succeeded.
     *
     * @param  list<array{job: mixed, delay: mixed, payload: string, native: array<string, mixed>}>  $messages
     * @return list<string>
     */
    protected function publishBatch(array $messages, mixed $queue): array
    {
        return tap(parent::publishBatch($messages, $queue), function () use ($messages, $queue): void {
            foreach ($messages as $message) {
                $this->event($this->queueName($queue), new JobPushed($message['payload']));
            }
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

    private function publishHorizonPayload(string $payload, ?string $queue, ?int $delayMs = null): string
    {
        $queueName = $this->queueName($queue);

        $result = $delayMs === null
            ? $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON])
            : $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON], $delayMs);

        $this->event($queueName, new JobPushed($payload));

        return $result;
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
