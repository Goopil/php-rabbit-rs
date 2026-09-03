<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Illuminate\Container\Container;

/**
 * Boots an additional provider instance with the native extension reported as
 * loaded (fakes are used in place of the extension) so connection compilation
 * can be observed at connection resolution.
 */
function bootedProviderWithFakeExtension(Container $app): RabbitMqServiceProvider
{
    $provider = new class($app) extends RabbitMqServiceProvider {
        protected function nativeExtensionLoaded(): bool
        {
            return true;
        }
    };
    $provider->register();
    $provider->boot();

    return $provider;
}

describe('RabbitMqServiceProvider', function () {
    it('reports the missing native extension when resolving the queue', function () {
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
        ]);

        expect(fn () => $this->app['queue']->connection('rabbit-rs'))
            ->toThrow(RuntimeException::class, 'ext-rabbit_rs');
    });

    it('boots with env-string boolean config and compiles at connection resolution', function () {
        $this->app['config']->set('rabbit-rs.best_effort', '1');
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);

        bootedProviderWithFakeExtension($this->app);

        expect($this->app['queue']->connection('rabbit-rs'))
            ->toBeInstanceOf(RabbitMqQueue::class);
    });

    it('defers config validation errors from boot to connection resolution', function () {
        $this->app['config']->set('rabbit-rs.best_effort', 'maybe');
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);

        bootedProviderWithFakeExtension($this->app);

        expect(fn () => $this->app['queue']->connection('rabbit-rs'))
            ->toThrow(InvalidArgumentException::class, 'best_effort');
    });

    it('compiles comma-separated hosts from the connection at resolution', function () {
        bootedProviderWithFakeExtension($this->app);
        $this->app['config']->set('queue.connections.rabbit-rs-hosts', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
            'hosts' => ' rabbit-a:5672, rabbit-b:5673 ',
        ]);

        $queue = $this->app['queue']->connection('rabbit-rs-hosts');
        // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
        $pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);

        expect($pool->config['brokers'][0]['hosts'])
            ->toBe([
                ['host' => 'rabbit-a', 'port' => 5672],
                ['host' => 'rabbit-b', 'port' => 5673],
            ]);
    });

    it('merges package defaults under the connection at resolution', function () {
        $this->app['config']->set('rabbit-rs.prefetch', '128');
        bootedProviderWithFakeExtension($this->app);
        $this->app['config']->set('queue.connections.rabbit-rs-defaults', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);

        $queue = $this->app['queue']->connection('rabbit-rs-defaults');
        // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
        $pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);

        expect($pool->config['workers'][0]['subscriptions'][0]['prefetch'])->toBe(128);
    });
});
