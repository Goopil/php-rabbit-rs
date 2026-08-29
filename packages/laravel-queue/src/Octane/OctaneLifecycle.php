<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Octane;

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
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
     * Called when Octane reloads the worker. All pools are flushed so
     * the next request creates fresh connections.
     */
    public function reload(): void
    {
        $this->closeConsumersOnResolvedQueues();
        $this->flushPoolFactory();
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

    private function closeConsumersOnResolvedQueues(): void
    {
        if (! $this->container->bound('queue')) {
            return;
        }

        $config = $this->container->make('config');
        $connections = $config->get('queue.connections', []);
        if (! is_array($connections)) {
            return;
        }

        $manager = $this->container->make('queue');

        foreach ($connections as $name => $connection) {
            if (! is_array($connection) || ($connection['driver'] ?? null) !== 'rabbit-rs') {
                continue;
            }

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
