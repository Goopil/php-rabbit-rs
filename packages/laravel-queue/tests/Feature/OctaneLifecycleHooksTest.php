<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature {
    use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
    use Goopil\RabbitRs\Laravel\Tests\TestCase;
    use Illuminate\Support\Facades\Event;

    final class OctaneLifecycleHooksTest extends TestCase
    {
        public function testServiceProviderRegistersReloadHookOnWorkerReloadEvent(): void
        {
            $events = $this->app->make('events');

            self::assertTrue(
                $events->hasListeners(\Laravel\Octane\Events\WorkerReload::class),
                'The service provider must listen for WorkerReload events when Octane is installed',
            );
        }

        public function testServiceProviderRegistersStopHookOnWorkerStoppingEvent(): void
        {
            $events = $this->app->make('events');

            self::assertTrue(
                $events->hasListeners(\Laravel\Octane\Events\WorkerStopping::class),
                'The service provider must listen for WorkerStopping events when Octane is installed',
            );
        }

        public function testWorkerReloadEventTriggersPoolFlush(): void
        {
            $factory = $this->app->make(NativePoolFactory::class);
            $config = $this->normalizedNativeConfig();
            $pool = $factory->make($config);

            Event::dispatch(new \Laravel\Octane\Events\WorkerReload());

            $poolAfterReload = $factory->make($config);
            self::assertNotSame($pool, $poolAfterReload, 'WorkerReload event must flush pools');
        }

        public function testWorkerStoppingEventTriggersPoolFlush(): void
        {
            $factory = $this->app->make(NativePoolFactory::class);
            $config = $this->normalizedNativeConfig();
            $pool = $factory->make($config);

            Event::dispatch(new \Laravel\Octane\Events\WorkerStopping());

            $poolAfterStop = $factory->make($config);
            self::assertNotSame($pool, $poolAfterStop, 'WorkerStopping event must flush pools');
        }

        public function testTerminatingCallbackIsRegistered(): void
        {
            $reflection = new \ReflectionClass($this->app);
            $property = $reflection->getProperty('terminatingCallbacks');
            $property->setAccessible(true);
            $callbacks = $property->getValue($this->app);

            self::assertNotEmpty($callbacks, 'A terminating callback must be registered for flush()');
        }

        /**
         * @return array<string, mixed>
         */
        private function normalizedNativeConfig(): array
        {
            $config = $this->app['config']->get('rabbit-rs');
            $normalized = \Goopil\RabbitRs\Laravel\Config\ConfigNormalizer::normalize(
                is_array($config) ? $config : [],
            );

            return $normalized['native'];
        }
    }
}

namespace Laravel\Octane {
    if (! class_exists(Octane::class, false)) {
        final class Octane {}
    }
}

namespace Laravel\Octane\Events {
    if (! class_exists(WorkerReload::class, false)) {
        final class WorkerReload {}
    }

    if (! class_exists(WorkerStopping::class, false)) {
        final class WorkerStopping {}
    }
}
