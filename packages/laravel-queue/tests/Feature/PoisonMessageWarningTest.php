<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Illuminate\Support\Facades\Log;

/**
 * Resets the once-per-process warning flag so each test observes it fresh.
 */
function resetUnboundedRedeliveryWarningFlag(): void
{
    $property = new ReflectionProperty(RabbitMqConnector::class, 'unboundedRedeliveryWarningEmitted');
    // @phpstan-ignore-next-line — intentionally accessing private static property for test isolation.
    $property->setValue(null, false);
}

/**
 * Connection config for the warning tests: production-shaped unless overridden.
 *
 * @param array<string, mixed> $overrides
 * @return array<string, mixed>
 */
function poisonConnectionConfig(array $overrides = []): array
{
    return array_merge([
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'production_warning' => true,
    ], $overrides);
}

/**
 * Arms the Log fake expecting no warning and resolves the given connection.
 */
function resolveWithoutWarning($app, string $connection = 'rabbit-rs'): void
{
    Log::shouldReceive('warning')->never();

    $app->make('queue')->connection($connection);
}

beforeEach(function (): void {
    resetUnboundedRedeliveryWarningFlag();
});

describe('poison-message production warning', function () {
    it('warns when delivery_limit and dead_letter are both unset in production', function () {
        bootFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs' => poisonConnectionConfig(),
        ]);
        Log::shouldReceive('warning')->once()->withArgs(
            fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
        );

        $this->app->make('queue')->connection('rabbit-rs');
    });

    it('does not warn when delivery_limit is configured', function () {
        config([
            'queue.connections.rabbit-rs' => poisonConnectionConfig([
                'delivery_limit' => 20,
                'dead_letter' => [
                    'exchange' => 'dlx.jobs',
                    'queue' => 'dead.jobs',
                ],
            ]),
        ]);
        bootFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        resolveWithoutWarning($this->app);
    });

    it('does not warn when production_warning is disabled', function () {
        bootFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs' => poisonConnectionConfig(['production_warning' => false]),
        ]);
        resolveWithoutWarning($this->app);
    });

    it('does not warn outside the production environment', function () {
        bootFakeNativeExtension($this->app);
        config([
            'queue.connections.rabbit-rs' => poisonConnectionConfig(),
        ]);
        resolveWithoutWarning($this->app);
    });

    it('warns only once per process for repeated resolutions', function () {
        bootFakeNativeExtension($this->app);
        $this->app->detectEnvironment(fn (): string => 'production');
        config([
            'queue.connections.rabbit-rs-a' => poisonConnectionConfig(),
            'queue.connections.rabbit-rs-b' => poisonConnectionConfig(),
        ]);
        Log::shouldReceive('warning')->once()->withArgs(
            fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
        );

        $queue = $this->app->make('queue');
        $queue->connection('rabbit-rs-a');
        $queue->connection('rabbit-rs-b');
    });
});
