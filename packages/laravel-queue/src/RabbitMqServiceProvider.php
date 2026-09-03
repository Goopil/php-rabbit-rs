<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Console\RabbitMqStatusCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommandExtension;
use Goopil\RabbitRs\Laravel\Exceptions\MissingExtensionException;
use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Support\Arr;
use Illuminate\Support\Facades\Log;
use Illuminate\Support\ServiceProvider;

class RabbitMqServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(self::configPath(), 'rabbit-rs');
        $this->app->singleton(NativePoolFactory::class);
    }

    public function boot(): void
    {
        $this->registerQueueConnector();
        $this->commands([RabbitMqStatusCommand::class, RabbitMqWorkCommand::class]);
        $this->registerWorkCommandExtension();
        $this->registerOctaneLifecycle();

        $this->publishes([
            self::configPath() => config_path('rabbit-rs.php'),
        ], 'rabbit-rs-config');
    }

    public function assertNativeExtensionLoaded(): void
    {
        if (! $this->nativeExtensionLoaded()) {
            self::throwMissingNativeExtension();
        }
    }

    protected function nativeExtensionLoaded(): bool
    {
        return extension_loaded('rabbit_rs');
    }

    private function registerQueueConnector(): void
    {
        $app = $this->app;
        $pools = $this->app->make(NativePoolFactory::class);
        $nativeExtensionLoaded = $this->nativeExtensionLoaded();

        $this->app->make('queue')->extend(
            'rabbit-rs',
            static function () use ($app, $nativeExtensionLoaded, $pools): RabbitMqConnector {
                if (! $nativeExtensionLoaded) {
                    self::throwMissingNativeExtension();
                }

                // Compilation is deferred to connection resolution: each
                // queue connection is compiled lazily from current config,
                // with this package config merged under it as defaults.
                $config = $app->make('config')->get('rabbit-rs');
                $defaults = Arr::except(is_array($config) ? $config : [], ['brokers', 'routes', 'workers']);

                return new RabbitMqConnector(
                    $pools,
                    $defaults,
                    inProductionEnvironment: static fn (): bool => $app->environment('production'),
                    productionWarningEnabled: (bool) (is_array($config) ? ($config['production_warning'] ?? true) : true),
                );
            },
        );
    }

    /**
     * Register the WorkCommand extension so that supervised `queue:work`
     * children tag their logs with the worker index from RABBIT_RS_WORKER.
     */
    private function registerWorkCommandExtension(): void
    {
        $extension = RabbitMqWorkCommandExtension::fromEnvironment();
        if ($extension->workerIndex() === null) {
            return;
        }

        $events = $this->app->make('events');
        $extension->register($events, static function (string $level, array $context): void {
            Log::channel()->{$level}('rabbit-rs worker', $context);
        });
    }

    private static function throwMissingNativeExtension(): never
    {
        throw new MissingExtensionException(
            'The Rabbit RS Laravel driver requires ext-rabbit_rs ^0.0 to be loaded.',
        );
    }

    private static function configPath(): string
    {
        return dirname(__DIR__).'/config/rabbit-rs.php';
    }

    private function registerOctaneLifecycle(): void
    {
        if (! class_exists(\Laravel\Octane\Octane::class)) {
            return;
        }

        $app = $this->app;
        $lifecycle = new OctaneLifecycle($app);

        $app->terminating(static fn () => $lifecycle->flush());

        $events = $app->make('events');
        $events->listen(\Laravel\Octane\Events\WorkerReload::class, static fn () => $lifecycle->reload());
        $events->listen(\Laravel\Octane\Events\WorkerStopping::class, static fn () => $lifecycle->stop());
    }
}
