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

    private static function configPath(): string
    {
        return dirname(__DIR__).'/config/rabbit-rs.php';
    }
}