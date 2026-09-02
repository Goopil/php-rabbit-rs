<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Http;

function bootProviderWithFakeExtension($app): void
{
    (new class($app) extends RabbitMqServiceProvider {
        protected function nativeExtensionLoaded(): bool
        {
            return true;
        }
    })->boot();
}

function cutoverQueuePool(object $queue): Pool
{
    // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
    return (new ReflectionProperty($queue, 'pool'))->getValue($queue);
}

beforeEach(function (): void {
    bootProviderWithFakeExtension($this->app);
});

describe('connection-first config cutover', function () {
    it('resolves two connections with different brokers into distinct pools', function (): void {
        config()->set('queue.connections.eu', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'eu-rabbit:5672',
            'vhost' => '/orders-eu',
        ]);
        config()->set('queue.connections.us', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'us-rabbit:5672',
            'vhost' => '/orders-us',
        ]);

        $manager = $this->app->make('queue');
        $euPool = cutoverQueuePool($manager->connection('eu'));
        $usPool = cutoverQueuePool($manager->connection('us'));

        expect($euPool)->not->toBe($usPool)
            ->and($euPool->config['brokers'][0]['hosts'][0]['host'])->toBe('eu-rabbit')
            ->and($usPool->config['brokers'][0]['hosts'][0]['host'])->toBe('us-rabbit')
            ->and($euPool->config['brokers'][0]['name'])->toBe('eu')
            ->and($usPool->config['brokers'][0]['name'])->toBe('us');
    });

    it('shares one pool between connections with byte-identical config', function (): void {
        $config = [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'shared-rabbit:5672',
        ];
        config()->set('queue.connections.primary', $config);
        config()->set('queue.connections.secondary', $config);

        $manager = $this->app->make('queue');
        $primaryPool = cutoverQueuePool($manager->connection('primary'));
        $secondaryPool = cutoverQueuePool($manager->connection('secondary'));

        expect($primaryPool)->toBe($secondaryPool);
    });

    it('compiles env-string scalars from a connection', function (): void {
        config()->set('queue.connections.envy', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'env-rabbit:5672,env-rabbit-b:5673',
            'vhost' => '/env',
            'heartbeat' => '15',
            'prefetch' => '32',
            'safety' => 'unsafe',
            'confirm_timeout' => '5000',
        ]);

        $manager = $this->app->make('queue');
        $pool = cutoverQueuePool($manager->connection('envy'));

        expect($pool->config['brokers'][0]['hosts'])->toBe([
            ['host' => 'env-rabbit', 'port' => 5672],
            ['host' => 'env-rabbit-b', 'port' => 5673],
        ])
            ->and($pool->config['brokers'][0]['heartbeat'])->toBe(15)
            ->and($pool->config['workers'][0]['subscriptions'][0]['prefetch'])->toBe(32)
            ->and($pool->config['publisher']['safety'])->toBe('unsafe')
            ->and($pool->config['publisher']['confirms'])->toBeTrue()
            ->and($pool->config['publisher']['mandatory'])->toBeFalse()
            ->and($pool->config['publisher']['confirm_timeout'])->toBe(5000);
    });

    it('reads the management url from the queue connection', function (): void {
        config()->set('queue.connections.statusy', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
            'management_url' => 'https://mq.local:15672',
        ]);
        Http::fake(['*' => Http::response([
            'messages_delivered' => 3,
            'messages_acked' => 2,
            'messages_redelivered' => 1,
        ])]);

        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful()
            ->expectsOutputToContain('"messages_delivered": 3');

        Http::assertSent(
            fn ($request) => str_starts_with($request->url(), 'https://mq.local:15672/api/queues/'),
        );
    });
});
