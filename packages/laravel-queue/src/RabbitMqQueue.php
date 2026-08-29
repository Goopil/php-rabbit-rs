<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\Support\MessageMapper;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Queue\Queue as QueueContract;
use Illuminate\Queue\Attributes\Delay;
use Illuminate\Queue\Queue;
use InvalidArgumentException;

class RabbitMqQueue extends Queue implements QueueContract
{
    /** @var array<string, Consumer> */
    private array $consumers = [];

    private MessageMapper $messages;

    /**
     * @param array<string, array<string, mixed>> $routes
     * @param array{confirm_timeout?: int} $publisherConfig
     */
    public function __construct(
        private readonly Pool $pool,
        private readonly array $routes,
        private readonly string $defaultQueue,
        bool $dispatchAfterCommit = false,
        ?MessageMapper $messages = null,
        private readonly WorkerProfileResolver $workerProfiles = new WorkerProfileResolver([]),
        private readonly int $blockForMilliseconds = 0,
        array $publisherConfig = [],
    ) {
        $this->dispatchAfterCommit = $dispatchAfterCommit;
        $this->messages = $messages ?? new MessageMapper($publisherConfig);
        $this->registerDefaultCallbacks();
    }

    private function registerDefaultCallbacks(): void
    {
        $weak = \WeakReference::create($this);
        $this->pool->onConnectionState(
            static function (string $broker, string $state, int $generation) use ($weak): void {
                $queue = $weak->get();
                if ($queue !== null) {
                    $queue->onConnectionState($broker, $state, $generation);
                }
            },
        );
        $this->pool->onBackpressure(
            static function (string $broker, int $inFlight, int $capacity) use ($weak): void {
                $queue = $weak->get();
                if ($queue !== null) {
                    $queue->onBackpressure($broker, $inFlight, $capacity);
                }
            },
        );
    }

    /**
     * Default handler for connection state changes, dispatching the
     * ConnectionStateChanged event through the Laravel event system.
     *
     * Register a custom callback via Pool::onConnectionState() to replace
     * this default behavior.
     */
    public function onConnectionState(string $broker, string $state, int $generation): void
    {
        app('events')->dispatch(new ConnectionStateChanged($broker, $state, $generation));
    }

    /**
     * Default handler for backpressure detection, dispatching the
     * BackpressureDetected event through the Laravel event system.
     *
     * Register a custom callback via Pool::onBackpressure() to replace
     * this default behavior.
     */
    public function onBackpressure(string $broker, int $inFlight, int $capacity): void
    {
        app('events')->dispatch(new BackpressureDetected($broker, $inFlight, $capacity));
    }

    public function size($queue = null)
    {
        $queueName = $this->queueName($queue);
        $route = $this->route($queueName);

        try {
            return $this->pool->size($route['broker'], $queueName);
        } catch (BackpressureException | ConnectionException $exception) {
            throw $exception;
        } catch (NativeException $exception) {
            throw QueueException::fromNative($exception);
        }
    }

    public function pendingSize($queue = null)
    {
        return $this->size($queue);
    }

    public function delayedSize($queue = null)
    {
        return 0;
    }

    public function reservedSize($queue = null)
    {
        return 0;
    }

    public function creationTimeOfOldestPendingJob($queue = null)
    {
        return null;
    }

    public function clear($queue = null): void
    {
        $queueName = $this->queueName($queue);
        $route = $this->route($queueName);

        try {
            $this->pool->clear($route['broker'], $queueName);
        } catch (BackpressureException | ConnectionException $exception) {
            throw $exception;
        } catch (NativeException $exception) {
            throw QueueException::fromNative($exception);
        }
    }

    public function push($job, $data = '', $queue = null)
    {
        $queueName = $this->queueName($queue);

        return $this->enqueueUsing(
            $job,
            $this->createPayload($job, $queueName, $data),
            $queue,
            null,
            fn (string $payload, ?string $queue): string => $this->publish(
                $payload,
                $queue,
                ['content_type' => 'application/json'],
            ),
        );
    }

    public function pushRaw($payload, $queue = null, array $options = [])
    {
        return $this->publish($payload, $queue, $options);
    }

    public function later($delay, $job, $data = '', $queue = null)
    {
        $queueName = $this->queueName($queue);

        return $this->enqueueUsing(
            $job,
            $this->createPayload($job, $queueName, $data, $delay),
            $queue,
            $delay,
            fn (string $payload, ?string $queue, mixed $delay): string => $this->publish(
                $payload,
                $queue,
                ['content_type' => 'application/json'],
                $this->delayMilliseconds($delay),
            ),
        );
    }

    /**
     * Publish a raw payload with a delay, bypassing enqueueUsing.
     * Used by Horizon subclass to dispatch with an already-prepared payload.
     */
    protected function laterRawFromPayload($delay, string $payload, $queue = null): string
    {
        return $this->publish(
            $payload,
            $queue,
            ['content_type' => 'application/json'],
            $this->delayMilliseconds($delay),
        );
    }

    public function bulk($jobs, $data = '', $queue = null)
    {
        $jobs = array_values((array) $jobs);
        if ($jobs === []) {
            return [];
        }

        [$afterCommit, $immediate] = $this->partitionJobsByAfterCommit($jobs);
        $messageIds = $immediate === []
            ? []
            : $this->publishBatch($this->prepareBatch($immediate, $data, $queue), $queue);

        if ($afterCommit !== []) {
            if (method_exists($this, 'registerRollbackCallbacksForJobsThatDispatchAfterCommit')) {
                foreach ($afterCommit as $job) {
                    $this->registerRollbackCallbacksForJobsThatDispatchAfterCommit($job);
                }
            }

            $messages = $this->prepareBatch($afterCommit, $data, $queue);
            $this->container->make('db.transactions')->addCallback(
                fn (): array => $this->publishBatch($messages, $queue),
            );
        }

        return $messageIds === [] ? null : $messageIds;
    }

    /**
     * @param list<mixed> $jobs
     * @return array{list<mixed>, list<mixed>}
     */
    protected function partitionJobsByAfterCommit(array $jobs): array
    {
        if (! $this->container->bound('db.transactions')) {
            return [[], $jobs];
        }

        $afterCommit = [];
        $immediate = [];
        foreach ($jobs as $job) {
            if ($this->shouldDispatchAfterCommit($job)) {
                $afterCommit[] = $job;
            } else {
                $immediate[] = $job;
            }
        }

        return [$afterCommit, $immediate];
    }

    /**
     * @param list<mixed> $jobs
     * @return list<array{job: mixed, delay: mixed, payload: string, native: array<string, mixed>}>
     */
    private function prepareBatch(array $jobs, mixed $data, mixed $queue): array
    {
        $queueName = $this->queueName($queue);
        $route = $this->route($queueName);

        return array_map(function (mixed $job) use ($data, $queueName, $route): array {
            $delay = $this->jobDelay($job);
            $payload = $this->createPayload($job, $queueName, $data, $delay);

            return [
                'job' => $job,
                'delay' => $delay,
                'payload' => $payload,
                'native' => $this->messages->map(
                    $payload,
                    $route,
                    $queueName,
                    ['content_type' => 'application/json'],
                    $delay === null ? null : $this->delayMilliseconds($delay),
                ),
            ];
        }, $jobs);
    }

    /**
     * @param list<array{job: mixed, delay: mixed, payload: string, native: array<string, mixed>}> $messages
     * @return list<string>
     */
    private function publishBatch(array $messages, mixed $queue): array
    {
        foreach ($messages as $message) {
            $this->raiseJobQueueingEvent(
                $queue,
                $message['job'],
                $message['payload'],
                $message['delay'],
            );
        }

        try {
            $messageIds = $this->pool->publishBatch(array_column($messages, 'native'));
        } catch (BackpressureException | ConnectionException $exception) {
            throw $exception;
        } catch (NativeException $exception) {
            throw QueueException::fromNative($exception);
        }

        foreach ($messages as $index => $message) {
            $this->raiseJobQueuedEvent(
                $queue,
                $messageIds[$index] ?? null,
                $message['job'],
                $message['payload'],
                $message['delay'],
            );
        }

        return $messageIds;
    }

    private function jobDelay(mixed $job): mixed
    {
        if (! is_object($job)) {
            return null;
        }

        if (method_exists($this, 'getAttributeValue') && class_exists(Delay::class)) {
            return $this->getAttributeValue($job, Delay::class, 'delay');
        }

        return $job->delay ?? null;
    }

    /**
     * Drains async settlement errors from all cached consumers and surfaces
     * connection-level failures as {@see ConnectionException}.
     *
     * Non-connection errors (e.g. AlreadySettled) are logged at warning level
     * when a container with a logger is available.  This method is called at
     * the top of {@see pop()} so that fire-and-forget settlement failures are
     * visible before the next delivery is fetched.
     */
    public function drainSettlementErrors(): void
    {
        foreach ($this->consumers as $consumer) {
            $errors = $consumer->drainErrors();
            foreach ($errors as $error) {
                $kind = $error['error_kind'] ?? '';
                if (in_array($kind, ['StaleGeneration', 'Transport'], true)) {
                    throw new ConnectionException($error['message'] ?? 'settlement error: ' . $kind);
                }
                if (isset($this->container)) {
                    $this->container->make('log')->warning('rabbit-rs settlement error', $error);
                }
            }
        }
    }

    public function pop($queue = null, $index = 0)
    {
        $this->drainSettlementErrors();

        if ($queue === null) {
            $profile = $this->workerProfiles->profileForQueue($this->defaultQueue)
                ?? $this->defaultQueue;
        } else {
            $queueName = $this->queueName($queue);
            $profile = $this->workerProfiles->profileForQueue($queueName);
            if ($profile === null) {
                throw new InvalidArgumentException("No worker profile subscribes to queue '{$queueName}'");
            }
        }
        try {
            $consumer = $this->consumers[$profile] ??= $this->pool->consumer($profile);
            $delivery = $consumer->next($this->blockForMilliseconds);
        } catch (ConnectionException $exception) {
            throw $exception;
        } catch (NativeException $exception) {
            throw QueueException::fromNative($exception);
        }
        if ($delivery === null) {
            return null;
        }
        $metadata = $delivery->metadata();

        return $this->marshalJob(
            $delivery,
            $this->workerProfiles->queue($profile, $metadata['subscription'] ?? null),
        );
    }

    /**
     * Closes all cached consumers and clears the cache.
     *
     * This prevents AMQP channel leaks in long-lived processes (Octane,
     * daemons) where consumers would otherwise accumulate across requests
     * or worker lifecycles without ever being closed.
     */
    public function closeConsumers(): void
    {
        foreach ($this->consumers as $consumer) {
            try {
                $consumer->close();
            } catch (NativeException) {
                // Best-effort: a closed or stale consumer is already cleaned up.
            }
        }
        $this->consumers = [];
    }

    public function __destruct()
    {
        $this->closeConsumers();
    }

    public function marshalJob(Delivery $delivery, $queue = null): RabbitMqJob
    {
        return new RabbitMqJob(
            $this->container,
            $delivery,
            $this->connectionName,
            $this->queueName($queue),
        );
    }

    /**
     * @param array<string, mixed> $options
     */
    private function publish(
        string $payload,
        ?string $queue,
        array $options,
        ?int $delayMilliseconds = null,
    ): string {
        $queueName = $this->queueName($queue);
        $message = $this->messages->map(
            $payload,
            $this->route($queueName),
            $queueName,
            $options,
            $delayMilliseconds,
        );

        try {
            return $this->pool->publish($message);
        } catch (BackpressureException | ConnectionException $exception) {
            throw $exception;
        } catch (NativeException $exception) {
            throw QueueException::fromNative($exception);
        }
    }

    protected function queueName(mixed $queue): string
    {
        $queue ??= $this->defaultQueue;
        if (! is_string($queue) || $queue === '') {
            throw new InvalidArgumentException('queue must be a non-empty string');
        }

        return $queue;
    }

    /**
     * @return array{broker: string, exchange: string, routing_key: string}
     */
    private function route(string $queue): array
    {
        $route = $this->routes[$queue] ?? $this->routes['default'] ?? null;
        if ($route === null) {
            throw new InvalidArgumentException("routes.{$queue} is not configured and no default route exists");
        }

        /** @var array{broker: string, exchange: string, routing_key: string} $route */
        return $route;
    }

    private function delayMilliseconds(mixed $delay): ?int
    {
        $seconds = max(0, $this->secondsUntil($delay));
        if ($seconds === 0) {
            return null;
        }

        if ($seconds > intdiv(PHP_INT_MAX, 1000)) {
            throw new InvalidArgumentException('delay exceeds the supported millisecond range');
        }

        return $seconds * 1000;
    }
}
