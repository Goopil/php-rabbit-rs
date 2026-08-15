<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Octane;

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Container\Container;

final class OctaneLifecycle
{
    public function __construct(
        private readonly Container $container,
    ) {}

    /**
     * Called after each request in Octane. The NativePoolFactory does not
     * retain request data, so this is a no-op for request-scoped state.
     */
    public function flush(): void
    {
        // NativePoolFactory does not retain request-scoped state.
    }

    /**
     * Called when Octane reloads the worker. All pools are flushed so
     * the next request creates fresh connections.
     */
    public function reload(): void
    {
        $this->flushPoolFactory();
    }

    /**
     * Called when the Octane worker stops. All pools are flushed.
     */
    public function stop(): void
    {
        $this->flushPoolFactory();
    }

    private function flushPoolFactory(): void
    {
        if ($this->container->bound(NativePoolFactory::class)) {
            $this->container->make(NativePoolFactory::class)->flush();
        }
    }
}
