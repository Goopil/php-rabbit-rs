<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Illuminate\Config\Repository;
use Illuminate\Container\Container;
use Illuminate\Contracts\Foundation\CachesConfiguration;
use RuntimeException;

final class RabbitMqServiceProviderTest extends TestCase
{
    public function testQueueResolutionReportsTheMissingNativeExtension(): void
    {
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
        ]);

        $this->expectException(RuntimeException::class);
        $this->expectExceptionMessage('ext-rabbit_rs');

        $this->app['queue']->connection('rabbit-rs');
    }

    public function testNormalizesCommaSeparatedHostsAfterConfigurationIsLoaded(): void
    {
        $this->app['config']->set(
            'rabbit-rs.brokers.default.hosts',
            ' rabbit-a:5672, , rabbit-b:5673 ',
        );

        (new RabbitMqServiceProvider($this->app))->register();

        self::assertSame(
            ['rabbit-a:5672', 'rabbit-b:5673'],
            $this->app['config']->get('rabbit-rs.brokers.default.hosts'),
        );
    }

    public function testNormalizesCommaSeparatedHostsWhenConfigurationIsCached(): void
    {
        $app = new class extends Container implements CachesConfiguration {
            public function configurationIsCached(): bool
            {
                return true;
            }

            public function getCachedConfigPath(): string
            {
                return '';
            }

            public function getCachedServicesPath(): string
            {
                return '';
            }
        };
        $app->instance('config', new Repository([
            'rabbit-rs' => [
                'brokers' => [
                    'default' => [
                        'hosts' => 'rabbit-a:5672,rabbit-b:5673',
                    ],
                ],
            ],
        ]));

        (new RabbitMqServiceProvider($app))->register();

        self::assertSame(
            ['rabbit-a:5672', 'rabbit-b:5673'],
            $app->make('config')->get('rabbit-rs.brokers.default.hosts'),
        );
    }

    public function testPreservesHostsAlreadyConfiguredAsAnArray(): void
    {
        $hosts = ['rabbit-a:5672', 'rabbit-b:5673'];
        $this->app['config']->set('rabbit-rs.brokers.default.hosts', $hosts);

        (new RabbitMqServiceProvider($this->app))->register();

        self::assertSame(
            $hosts,
            $this->app['config']->get('rabbit-rs.brokers.default.hosts'),
        );
    }

    public function testNormalizesAnEmptyHostsStringToAnEmptyList(): void
    {
        $this->app['config']->set('rabbit-rs.brokers.default.hosts', ' , ');

        (new RabbitMqServiceProvider($this->app))->register();

        self::assertSame(
            [],
            $this->app['config']->get('rabbit-rs.brokers.default.hosts'),
        );
    }
}