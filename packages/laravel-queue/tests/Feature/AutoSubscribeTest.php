<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;

/**
 * @return list<array<string, mixed>>
 */
function autoSubscribeWorkers(): array
{
    return [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'orders', 'queue' => 'orders-eu'],
            ],
        ],
    ];
}

/**
 * @return array<string, array<string, string>>
 */
function autoSubscribeRoutes(): array
{
    return [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => '',
            'routing_key' => '{queue}',
        ],
    ];
}

/**
 * Builds a queue whose resolver knows only the declared 'default' profile,
 * so pop('emails') exercises the unknown-queue rejection path.
 *
 * @return array{RabbitMqQueue, Pool}
 */
function makeAutoSubscribeQueue(bool $autoSubscribe): array
{
    $pool = new Pool(['workers' => autoSubscribeWorkers()]);
    $queue = new RabbitMqQueue(
        $pool,
        autoSubscribeRoutes(),
        'default',
        autoSubscribe: $autoSubscribe,
        workerProfiles: new WorkerProfileResolver(autoSubscribeWorkers()),
    );
    $queue->setContainer(new Container);
    $queue->setConnectionName('rabbit-rs');

    return [$queue, $pool];
}

/**
 * Builds a connector whose factory returns the given pool, mirroring the
 * integration test bootstrap without the native extension. The package
 * configuration (rabbit-rs defaults) feeds the compiler.
 */
function autoSubscribeConnector(Pool $pool): RabbitMqConnector
{
    $factory = new NativePoolFactory(createPool: static fn (): Pool => $pool);
    $config = app('config')->get('rabbit-rs');

    return new RabbitMqConnector(
        $factory,
        is_array($config) ? $config : [],
    );
}

describe('auto_subscribe pop', function () {
    it('keeps a worker profile name working on pop', function (): void {
        [$queue, $pool] = makeAutoSubscribeQueue(false);

        $queue->pop('default');

        expect(['default'])->toBe($pool->consumerProfiles);
    });

    it('rejects a plain queue not declared on the connection', function (): void {
        [$queue] = makeAutoSubscribeQueue(false);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage(
            "No worker profile subscribes to queue 'emails': declare it in "
            .'queue.connections.<name> (queue key or subscriptions).'
        );

        $queue->pop('emails');
    });
});

describe('auto_subscribe compile rejection', function () {
    it('rejects the option set at the connection level through the connector', function (): void {
        $pool = new Pool(['workers' => autoSubscribeWorkers()]);

        expect(fn () => autoSubscribeConnector($pool)->connect([
            'queue' => 'default',
            'auto_subscribe' => true,
        ]))->toThrow(
            InvalidArgumentException::class,
            'queue.connections.default.auto_subscribe: auto_subscribe is removed in v1: '
            .'runtime worker-profile registration is not supported — '
            .'declare the queue profile explicitly (queue key or subscriptions)',
        );
    });

    it('rejects the option arriving through the package defaults', function (): void {
        $this->app['config']->set('rabbit-rs.auto_subscribe', true);
        $pool = new Pool(['workers' => autoSubscribeWorkers()]);

        expect(fn () => autoSubscribeConnector($pool)->connect(['queue' => 'default']))
            ->toThrow(InvalidArgumentException::class, 'auto_subscribe is removed in v1');
    });

    it('rejects a junk string auto_subscribe', function (): void {
        $pool = new Pool(['workers' => autoSubscribeWorkers()]);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('auto_subscribe');

        autoSubscribeConnector($pool)->connect(['queue' => 'default', 'auto_subscribe' => 'maybe']);
    });

    it('rejects the key even when the stale default disables it', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('default', ['queue' => 'default'], ['auto_subscribe' => false]))
            ->toThrow(InvalidArgumentException::class, 'auto_subscribe is removed in v1');
    });
});
