<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Octane;

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Container\Container;

final class OctaneLifecycle
{
    public function __construct(
        private readonly Container $container,
    ) {}

    /**
     * Called after each request in Octane. Closes cached consumers on all
     * resolved RabbitMqQueue connections to prevent AMQP channel leaks across
     * requests.
     */
    public function flush(): void
    {
        $this->closeConsumersOnResolvedQueues();
    }

    /**
     * Called when Octane reloads the worker. All pools are flushed and the
     * queue manager's resolved connections are dropped, so the next request
     * recompiles every connection from the current config (broker/credential
     * rotation via env).
     */
    public function reload(): void
    {
        $this->closeConsumersOnResolvedQueues();
        $this->flushPoolFactory();
        $this->forgetResolvedConnections();
    }

    /**
     * Called when the Octane worker stops. All pools are flushed.
     *
     * This intentionally mirrors {@see reload()} — both operations require
     * a full flush, but stop() may diverge in the future (e.g. waiting for
     * in-flight work before flushing).
     */
    public function stop(): void
    {
        $this->closeConsumersOnResolvedQueues();
        $this->flushPoolFactory();
    }

    private function flushPoolFactory(): void
    {
        if ($this->container->bound(NativePoolFactory::class)) {
            $this->container->make(NativePoolFactory::class)->flush();
        }
    }

    /**
     * Drops the queue manager's resolved connections so the next resolution
     * recompiles each connection from the current config. Fails silent by
     * design: if the property ever disappears from the framework, a stale
     * pool merely survives until the next request's pool factory flush.
     *
     * ponytail: reflection clears QueueManager::$connections — no public API
     * exists in Laravel 13; switch to one if added upstream.
     */
    private function forgetResolvedConnections(): void
    {
        if (! $this->container->bound('queue')) {
            return;
        }

        try {
            $manager = $this->container->make('queue');
            $property = new \ReflectionProperty($manager, 'connections');
            $value = $property->getValue($manager);
            if (is_array($value)) {
                $property->setValue($manager, []);
            }
        } catch (\ReflectionException) {
            // Property gone (future Laravel core change): no-op.
        }
    }

    private function closeConsumersOnResolvedQueues(): void
    {
        if (! $this->container->bound('queue')) {
            return;
        }

        $manager = $this->container->make('queue');

        foreach (RabbitRsConnections::all() as $name => $connection) {
            if (! $manager->connected($name)) {
                continue;
            }

            $queue = $manager->connection($name);
            if ($queue instanceof RabbitMqQueue) {
                $queue->closeConsumers();
            }
        }
    }
}
