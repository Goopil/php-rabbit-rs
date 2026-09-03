<?php

declare(strict_types=1);

use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;
use Illuminate\Support\Facades\Event;

if (! function_exists('lifecycleCompiledNativeConfig')) {
    function lifecycleCompiledNativeConfig($app): array
    {
        $compiled = \Goopil\RabbitRs\Laravel\Config\ConnectionCompiler::compile(
            'rabbit-rs',
            ['queue' => 'default'],
            is_array($app['config']->get('rabbit-rs')) ? $app['config']->get('rabbit-rs') : [],
        );

        return $compiled['native'];
    }
}

function resolveQueueWithConsumer($app): array
{
    $workers = [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'orders', 'queue' => 'orders-eu'],
            ],
        ],
    ];
    $routes = [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => '',
            'routing_key' => '{queue}',
        ],
    ];

    $pool = new Pool(['workers' => $workers]);
    $resolver = new WorkerProfileResolver($workers);
    $queue = new RabbitMqQueue(
        $pool,
        $routes,
        'default',
        workerProfiles: $resolver,
        blockForMilliseconds: 0,
    );
    $queue->setContainer($app);
    $queue->setConnectionName('rabbit-rs');

    // Register the connection config so the manager knows about it.
    $app['config']->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
    ]);

    // Register the resolved connection so the manager returns our queue.
    $manager = $app->make('queue');
    $reflection = new \ReflectionClass($manager);
    $connectionsProperty = $reflection->getProperty('connections');
    // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
    $connectionsProperty->setValue($manager, ['rabbit-rs' => $queue]);

    // Trigger consumer creation by calling pop().
    $queue->pop('orders-eu');

    return [$queue, $pool];
}

describe('pool reuse', function () {
    it('two requests reuse the same pool in one worker', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = lifecycleCompiledNativeConfig($this->app);

        $pool1 = $factory->make($config);
        $pool2 = $factory->make($config);

        expect($pool1)->toBe($pool2);
    });

    it('no request state is retained in pool', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = lifecycleCompiledNativeConfig($this->app);

        $pool = $factory->make($config);
        $reflection = new \ReflectionClass($pool);
        $properties = array_map(fn (\ReflectionProperty $p): string => $p->getName(), $reflection->getProperties());

        expect($properties)->not->toContain('request')
            ->and($properties)->not->toContain('requestId');
    });

    it('pool is independent per worker', function () {
        $factory1 = new NativePoolFactory();
        $factory2 = new NativePoolFactory();
        $config = lifecycleCompiledNativeConfig($this->app);

        $pool1 = $factory1->make($config);
        $pool2 = $factory2->make($config);

        expect($pool1)->not->toBe($pool2);
    });
});

describe('lifecycle operations', function () {
    it('OctaneLifecycle can be constructed without Octane installed', function () {
        $lifecycle = new OctaneLifecycle($this->app);

        expect($lifecycle)->toBeInstanceOf(OctaneLifecycle::class);
    });

    it('flush does not recreate the pool', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = lifecycleCompiledNativeConfig($this->app);

        $pool = $factory->make($config);
        expect($factory->make($config))->toBe($pool);

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->flush();

        $poolAfterFlush = $factory->make($config);
        expect($poolAfterFlush)->toBe($pool);
    });

    it('reload closes all pools', function () {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = lifecycleCompiledNativeConfig($this->app);

        $pool = $factory->make($config);
        expect($factory->make($config))->toBe($pool);

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->reload();

        $poolAfterReload = $factory->make($config);
        expect($poolAfterReload)->not->toBe($pool);
    });

    it('reload calls close on the cached pool', function () {
        [$pool, $factory] = trackedPoolFactory($this->app);

        $factory->make(lifecycleCompiledNativeConfig($this->app));

        (new OctaneLifecycle($this->app))->reload();

        expect($pool->closeCalls)->toBe(1);
    });

    it('worker stop drains pools', function () {
        $lifecycle = new OctaneLifecycle($this->app);

        $factory = $this->app->make(NativePoolFactory::class);
        $config = lifecycleCompiledNativeConfig($this->app);
        $pool = $factory->make($config);

        $lifecycle->stop();

        $poolAfterStop = $factory->make($config);
        expect($poolAfterStop)->not->toBe($pool);
    });

    it('worker stop calls close on the cached pool', function () {
        [$pool, $factory] = trackedPoolFactory($this->app);

        $factory->make(lifecycleCompiledNativeConfig($this->app));

        (new OctaneLifecycle($this->app))->stop();

        expect($pool->closeCalls)->toBe(1);
    });

    it('flush does not close pools', function () {
        [$pool, $factory] = trackedPoolFactory($this->app);

        $factory->make(lifecycleCompiledNativeConfig($this->app));

        (new OctaneLifecycle($this->app))->flush();

        expect($pool->closeCalls)->toBe(0);
    });

    it('flush without queue manager does not throw', function () {
        $container = new Container();
        $lifecycle = new OctaneLifecycle($container);

    // Should not throw even though 'queue' is not bound.
        $lifecycle->flush();

        expect(true)->toBeTrue();
    });
});

describe('config refresh on reload', function () {
    it('reload drops resolved connections so the same name recompiles from fresh config', function () {
        $app = $this->app;
        $app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);

        [$pool, $poolAfter] = resolveRotateAndReload($app, 'rabbit-rs');

        expect($poolAfter->config['brokers'][0]['hosts'][0]['host'])->toBe('rotated')
            ->and($poolAfter)->not->toBe($pool);
    });

    it('reload propagates config changes to newly resolved queue connections', function () {
        $app = $this->app;
        $app['config']->set('queue.connections.rabbit-rs-rotated', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
        ]);

        [$pool] = resolveRotateAndReload($app, 'rabbit-rs-rotated');

        $app['config']->set('queue.connections.rabbit-rs-fresh', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
            'hosts' => 'rotated:5672',
        ]);
        $poolAfter = octaneQueuePool($app['queue']->connection('rabbit-rs-fresh'));

        expect($poolAfter->config['brokers'][0]['hosts'][0]['host'])->toBe('rotated')
            ->and($poolAfter)->not->toBe($pool);
    });
});

/**
 * Reads the native pool held by a resolved RabbitMqQueue.
 */
function octaneQueuePool(object $queue): Pool
{
    // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
    return (new ReflectionProperty($queue, 'pool'))->getValue($queue);
}

/**
 * Binds a NativePoolFactory whose make() always returns a tracked Pool.
 *
 * @return array{0: Pool, 1: NativePoolFactory}
 */
function trackedPoolFactory($app): array
{
    $pool = new Pool();
    $factory = new NativePoolFactory(
        createPool: static fn (array $config): Pool => $pool,
    );
    $app->instance(NativePoolFactory::class, $factory);

    return [$pool, $factory];
}

/**
 * Boots the fake-extension provider, resolves the given connection, then
 * rotates its hosts config and dispatches the Octane reload event.
 *
 * @return array{0: Pool, 1: Pool} pools resolved before and after the reload
 */
function resolveRotateAndReload($app, string $connection): array
{
    bootFakeNativeExtension($app);

    $pool = octaneQueuePool($app['queue']->connection($connection));
    expect($pool->config['brokers'][0]['hosts'][0]['host'])->toBe('127.0.0.1');

    $app['config']->set("queue.connections.{$connection}.hosts", 'rotated:5672');
    Event::dispatch(new \Laravel\Octane\Events\WorkerReload());

    return [$pool, octaneQueuePool($app['queue']->connection($connection))];
}

describe('consumer cleanup', function () {
    it('flush closes consumers on current queue', function () {
        [, $pool] = resolveQueueWithConsumer($this->app);
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->flush();

        expect($consumer->closeCalls)->toBe(1);
    });

    it('reload closes consumers on current queue', function () {
        [, $pool] = resolveQueueWithConsumer($this->app);
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->reload();

        expect($consumer->closeCalls)->toBe(1);
    });

    it('stop closes consumers on current queue', function () {
        [, $pool] = resolveQueueWithConsumer($this->app);
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->stop();

        expect($consumer->closeCalls)->toBe(1);
    });
});
