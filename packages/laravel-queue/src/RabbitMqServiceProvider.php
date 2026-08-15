<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Console\RabbitMqStatusCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand;
use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Support\ServiceProvider;
use RuntimeException;

class RabbitMqServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(self::configPath(), 'rabbit-rs');
        $this->normalizeBrokerHosts();
        $this->app->singleton(NativePoolFactory::class);
        $this->app->singleton('rabbit-rs.config', fn (): array => ConfigNormalizer::normalize(
            is_array($this->app->make('config')->get('rabbit-rs')) ? $this->app->make('config')->get('rabbit-rs') : [],
        ));
    }

    public function boot(): void
    {
        $this->registerQueueConnector();
        $this->commands([RabbitMqStatusCommand::class, RabbitMqWorkCommand::class]);
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
        $config = $this->app->make('config')->get('rabbit-rs');
        $normalizedConfig = ConfigNormalizer::normalize(is_array($config) ? $config : []);
        $pools = $this->app->make(NativePoolFactory::class);
        $nativeExtensionLoaded = $this->nativeExtensionLoaded();

        $this->app->make('queue')->extend(
            'rabbit-rs',
            static function () use ($nativeExtensionLoaded, $normalizedConfig, $pools): RabbitMqConnector {
                if (! $nativeExtensionLoaded) {
                    self::throwMissingNativeExtension();
                }

                return new RabbitMqConnector($pools, $normalizedConfig);
            },
        );
    }

    private function normalizeBrokerHosts(): void
    {
        $config = $this->app->make('config');
        $brokers = $config->get('rabbit-rs.brokers');

        if (! is_array($brokers)) {
            return;
        }

        foreach ($brokers as &$broker) {
            if (is_array($broker) && isset($broker['hosts']) && is_string($broker['hosts'])) {
                $broker['hosts'] = self::parseHosts($broker['hosts']);
            }
        }
        unset($broker);

        $config->set('rabbit-rs.brokers', $brokers);
    }

    /**
     * @return list<string>
     */
    private static function parseHosts(string $hosts): array
    {
        return array_values(array_filter(
            array_map('trim', explode(',', $hosts)),
            static fn (string $host): bool => $host !== '',
        ));
    }

    private static function throwMissingNativeExtension(): never
    {
        throw new RuntimeException(
            'The Rabbit RS Laravel driver requires ext-rabbit_rs ^1.0 to be loaded.',
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
    }
}