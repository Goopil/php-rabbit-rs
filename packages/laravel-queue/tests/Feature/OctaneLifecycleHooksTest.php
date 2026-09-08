<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Support\Facades\Event;
use Laravel\Octane\Events\WorkerReload;
use Laravel\Octane\Events\WorkerStopping;

function hooksCompiledNativeConfig($app): array
{
    $compiled = ConnectionCompiler::compile(
        'rabbit-rs',
        ['queue' => 'default'],
        is_array($app['config']->get('rabbit-rs')) ? $app['config']->get('rabbit-rs') : [],
    );

    return $compiled['native'];
}

describe('Octane lifecycle hooks', function () {
    it('service provider registers reload hook on WorkerReload event', function () {
        $events = $this->app->make('events');

        expect($events->hasListeners(WorkerReload::class))->toBeTrue();
    });

    it('service provider registers stop hook on WorkerStopping event', function () {
        $events = $this->app->make('events');

        expect($events->hasListeners(WorkerStopping::class))->toBeTrue();
    });

    it('WorkerReload event triggers pool flush', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = hooksCompiledNativeConfig($this->app);
        $pool = $factory->make($config);

        Event::dispatch(new WorkerReload);

        $poolAfterReload = $factory->make($config);
        expect($poolAfterReload)->not->toBe($pool);
    });

    it('WorkerStopping event triggers pool flush', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = hooksCompiledNativeConfig($this->app);
        $pool = $factory->make($config);

        Event::dispatch(new WorkerStopping);

        $poolAfterStop = $factory->make($config);
        expect($poolAfterStop)->not->toBe($pool);
    });

    it('terminating callback is registered', function () {
        $reflection = new ReflectionClass($this->app);
        $property = $reflection->getProperty('terminatingCallbacks');
        // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
        $property->setAccessible(true);
        // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
        $callbacks = $property->getValue($this->app);

        expect($callbacks)->not->toBeEmpty();
    });
});
