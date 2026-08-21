<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Illuminate\Config\Repository;
use Illuminate\Container\Container;
use Illuminate\Contracts\Foundation\CachesConfiguration;
use RuntimeException;

describe('RabbitMqServiceProvider', function () {
    it('reports the missing native extension when resolving the queue', function () {
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
        ]);

        expect(fn () => $this->app['queue']->connection('rabbit-rs'))
            ->toThrow(RuntimeException::class, 'ext-rabbit_rs');
    });

    it('normalizes comma-separated hosts after configuration is loaded', function () {
        $this->app['config']->set(
            'rabbit-rs.brokers.default.hosts',
            ' rabbit-a:5672, , rabbit-b:5673 ',
        );

        (new RabbitMqServiceProvider($this->app))->register();

        expect($this->app['config']->get('rabbit-rs.brokers.default.hosts'))
            ->toBe(['rabbit-a:5672', 'rabbit-b:5673']);
    });

    it('normalizes comma-separated hosts when configuration is cached', function () {
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

        expect($app->make('config')->get('rabbit-rs.brokers.default.hosts'))
            ->toBe(['rabbit-a:5672', 'rabbit-b:5673']);
    });

    it('preserves hosts already configured as an array', function () {
        $hosts = ['rabbit-a:5672', 'rabbit-b:5673'];
        $this->app['config']->set('rabbit-rs.brokers.default.hosts', $hosts);

        (new RabbitMqServiceProvider($this->app))->register();

        expect($this->app['config']->get('rabbit-rs.brokers.default.hosts'))
            ->toBe($hosts);
    });

    it('normalizes an empty hosts string to an empty list', function () {
        $this->app['config']->set('rabbit-rs.brokers.default.hosts', ' , ');

        (new RabbitMqServiceProvider($this->app))->register();

        expect($this->app['config']->get('rabbit-rs.brokers.default.hosts'))
            ->toBe([]);
    });
});
