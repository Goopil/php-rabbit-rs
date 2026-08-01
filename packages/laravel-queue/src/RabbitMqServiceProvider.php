<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel;

use Illuminate\Support\ServiceProvider;
use RuntimeException;

class RabbitMqServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(self::configPath(), 'rabbit-rs');
        $this->normalizeBrokerHosts();
    }

    public function boot(): void
    {
        $this->publishes([
            self::configPath() => config_path('rabbit-rs.php'),
        ], 'rabbit-rs-config');
    }

    public function assertNativeExtensionLoaded(): void
    {
        if (! $this->nativeExtensionLoaded()) {
            throw new RuntimeException(
                'The Rabbit RS Laravel driver requires ext-rabbit_rs ^1.0 to be loaded.',
            );
        }
    }

    protected function nativeExtensionLoaded(): bool
    {
        return extension_loaded('rabbit_rs');
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

    private static function configPath(): string
    {
        return dirname(__DIR__).'/config/rabbit-rs.php';
    }
}