<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Illuminate\Support\Facades\Log;

/**
 * Boots a second provider instance whose extension check succeeds, so the
 * queue connection resolves end-to-end with the fake Pool classes from the
 * test bootstrap (Feature tests run without the compiled extension).
 */
function bootProviderWithFakeNativeExtension($app): void
{
    (new class($app) extends RabbitMqServiceProvider {
        protected function nativeExtensionLoaded(): bool
        {
            return true;
        }
    })->boot();
}

/**
 * Resets the once-per-process warning flag so each test observes it fresh.
 */
function resetUnboundedRedeliveryWarningFlag(): void
{
    $property = new ReflectionProperty(RabbitMqConnector::class, 'unboundedRedeliveryWarningEmitted');
    // @phpstan-ignore-next-line — intentionally accessing private static property for test isolation.
    $property->setValue(null, false);
}

beforeEach(function (): void {
    resetUnboundedRedeliveryWarningFlag();
});

describe('poison-message production warning', function () {
    it('warns when delivery_limit and dead_letter are both unset in production', function () {
        bootProviderWithFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => true,
            ],
        ]);
        Log::shouldReceive('warning')->once()->withArgs(
            fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
        );

        $this->app->make('queue')->connection('rabbit-rs');
    });

    it('does not warn when delivery_limit is configured', function () {
        config([
            'queue.connections.rabbit-rs' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => true,
                'delivery_limit' => 20,
                'dead_letter' => [
                    'exchange' => 'dlx.jobs',
                    'queue' => 'dead.jobs',
                ],
            ],
        ]);
        bootProviderWithFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        Log::shouldReceive('warning')->never();

        $this->app->make('queue')->connection('rabbit-rs');
    });

    it('does not warn when production_warning is disabled', function () {
        bootProviderWithFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => false,
            ],
        ]);
        Log::shouldReceive('warning')->never();

        $this->app->make('queue')->connection('rabbit-rs');
    });

    it('does not warn outside the production environment', function () {
        bootProviderWithFakeNativeExtension($this->app);
        config([
            'queue.connections.rabbit-rs' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => true,
            ],
        ]);
        Log::shouldReceive('warning')->never();

        $this->app->make('queue')->connection('rabbit-rs');
    });

    it('warns only once per process for repeated resolutions', function () {
        bootProviderWithFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs-a' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => true,
            ],
            'queue.connections.rabbit-rs-b' => [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
                'production_warning' => true,
            ],
        ]);
        Log::shouldReceive('warning')->once()->withArgs(
            fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
        );

        $queue = $this->app->make('queue');
        $queue->connection('rabbit-rs-a');
        $queue->connection('rabbit-rs-b');
    });
});
