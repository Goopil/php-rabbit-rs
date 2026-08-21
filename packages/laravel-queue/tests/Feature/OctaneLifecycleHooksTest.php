<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Support\Facades\Event;

function hooksNormalizedNativeConfig($app): array
{
    $config = $app['config']->get('rabbit-rs');
    $normalized = \Goopil\RabbitRs\Laravel\Config\ConfigNormalizer::normalize(
        is_array($config) ? $config : [],
    );

    return $normalized['native'];
}

describe('Octane lifecycle hooks', function () {
    it('service provider registers reload hook on WorkerReload event', function () {
        $events = $this->app->make('events');

        expect($events->hasListeners(\Laravel\Octane\Events\WorkerReload::class))->toBeTrue();
    });

    it('service provider registers stop hook on WorkerStopping event', function () {
        $events = $this->app->make('events');

        expect($events->hasListeners(\Laravel\Octane\Events\WorkerStopping::class))->toBeTrue();
    });

    it('WorkerReload event triggers pool flush', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = hooksNormalizedNativeConfig($this->app);
        $pool = $factory->make($config);

        Event::dispatch(new \Laravel\Octane\Events\WorkerReload());

        $poolAfterReload = $factory->make($config);
        expect($poolAfterReload)->not->toBe($pool);
    });

    it('WorkerStopping event triggers pool flush', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = hooksNormalizedNativeConfig($this->app);
        $pool = $factory->make($config);

        Event::dispatch(new \Laravel\Octane\Events\WorkerStopping());

        $poolAfterStop = $factory->make($config);
        expect($poolAfterStop)->not->toBe($pool);
    });

    it('terminating callback is registered', function () {
        $reflection = new \ReflectionClass($this->app);
        $property = $reflection->getProperty('terminatingCallbacks');
        $property->setAccessible(true);
        $callbacks = $property->getValue($this->app);

        expect($callbacks)->not->toBeEmpty();
    });
});
