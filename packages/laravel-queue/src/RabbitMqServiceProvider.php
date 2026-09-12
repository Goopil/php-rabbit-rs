<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Console\RabbitMqDoctorCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqProbeCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqStatusCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqTopologyCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommandExtension;
use Goopil\RabbitRs\Laravel\Exceptions\MissingExtensionException;
use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\ProbeStatefile;
use Illuminate\Queue\Events\WorkerStopping as QueueWorkerStopping;
use Illuminate\Support\Arr;
use Illuminate\Support\ServiceProvider;
use Laravel\Octane\Events\WorkerReload;
use Laravel\Octane\Events\WorkerStopping;
use Laravel\Octane\Octane;

class RabbitMqServiceProvider extends ServiceProvider
{
    /**
     * Version constraint of the native ext-rabbit_rs extension, enforced at
     * connection resolution. The `ext-rabbit_rs` suggest entry in
     * composer.json must reference this constraint.
     */
    public const EXTENSION_CONSTRAINT = '^0.3.1';

    public function register(): void
    {
        $this->mergeConfigFrom(self::configPath(), 'rabbit-rs');
        $this->app->singleton(NativePoolFactory::class);
        $this->app->singleton(ProbeStatefile::class, static function ($app): ProbeStatefile {
            $path = $app->make('config')->get('rabbit-rs.probes.path')
                ?? storage_path('framework/rabbit-rs/probes');

            return new ProbeStatefile((string) $path, (int) getmypid());
        });
    }

    public function boot(): void
    {
        $this->registerQueueConnector();
        $this->commands([RabbitMqStatusCommand::class, RabbitMqWorkCommand::class, RabbitMqDoctorCommand::class, RabbitMqTopologyCommand::class, RabbitMqProbeCommand::class]);
        $this->registerWorkCommandExtension();
        $this->registerWorkerStoppingProbe();
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
     * children tag their logs with the worker index from RABBIT_RS_WORKER_INDEX.
     */
    private function registerWorkCommandExtension(): void
    {
        RabbitMqWorkCommandExtension::fromEnvironment()
            ->registerWithLog($this->app->make('events'));
    }

    /**
     * Flip the worker's probe statefile to draining when a queue worker stops
     * (SIGTERM handled by queue:work, or --max-jobs recycling), so that
     * `rabbit-rs:probe prestop` sees the drain.
     */
    private function registerWorkerStoppingProbe(): void
    {
        $this->app->make('events')->listen(QueueWorkerStopping::class, static function (): void {
            if (app()->bound(ProbeStatefile::class)) {
                app(ProbeStatefile::class)->draining();
            }
        });
    }

    private static function throwMissingNativeExtension(): never
    {
        throw new MissingExtensionException(
            sprintf(
                'The Rabbit RS Laravel driver requires ext-rabbit_rs %s to be loaded. Install it with `pie install goopil/rabbit-rs-native` (macOS: `brew install goopil/rabbit-rs/rabbit-rs`), then retry.',
                self::EXTENSION_CONSTRAINT,
            ),
        );
    }

    private static function configPath(): string
    {
        return dirname(__DIR__).'/config/rabbit-rs.php';
    }

    private function registerOctaneLifecycle(): void
    {
        if (! class_exists(Octane::class)) {
            return;
        }

        $app = $this->app;
        $lifecycle = new OctaneLifecycle($app);

        $app->terminating(static fn () => $lifecycle->flush());

        $events = $app->make('events');
        $events->listen(WorkerReload::class, static fn () => $lifecycle->reload());
        $events->listen(WorkerStopping::class, static fn () => $lifecycle->stop());
    }
}
