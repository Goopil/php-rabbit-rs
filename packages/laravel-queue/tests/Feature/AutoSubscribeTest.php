<?php

declare(strict_types=1);

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
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
 * A fake pool seeded with the implicit worker profile that auto-subscribe
 * requests ('__auto__.emails'), mirroring a native pool able to resolve
 * runtime-registered profiles.
 */
function autoSubscribeSeededPool(): Pool
{
    return new Pool(['workers' => [...autoSubscribeWorkers(), [
        'name' => '__auto__.emails',
        'subscriptions' => [
            ['name' => 'auto', 'queue' => 'emails'],
        ],
    ]]]);
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
 * Builds a queue whose pool is seeded with the implicit worker profile that
 * auto-subscribe requests. The resolver deliberately does not know the
 * 'emails' queue so pop('emails') exercises the auto-subscribe path.
 *
 * @return array{RabbitMqQueue, Pool}
 */
function makeAutoSubscribeQueue(bool $autoSubscribe): array
{
    $pool = autoSubscribeSeededPool();
    $queue = new RabbitMqQueue(
        $pool,
        autoSubscribeRoutes(),
        'default',
        autoSubscribe: $autoSubscribe,
        workerProfiles: new WorkerProfileResolver(autoSubscribeWorkers()),
    );
    $queue->setContainer(new Container());
    $queue->setConnectionName('rabbit-rs');

    return [$queue, $pool];
}

/**
 * Builds a connector whose factory returns the given pool, mirroring the
 * integration test bootstrap without the native extension. The merged package
 * configuration (including test overrides of rabbit-rs.*) feeds the normalizer.
 */
function autoSubscribeConnector(Pool $pool): RabbitMqConnector
{
    $factory = new NativePoolFactory(createPool: static fn (): Pool => $pool);
    $config = app('config')->get('rabbit-rs');

    return new RabbitMqConnector(
        $factory,
        ConfigNormalizer::normalize(is_array($config) ? $config : []),
    );
}

/**
 * @return array<string, mixed>
 */
function validAutoSubscribeConfig(): array
{
    return [
        'topology_mode' => 'declare',
        'brokers' => [
            'default' => [
                'hosts' => ['127.0.0.1:5672'],
                'vhost' => '/',
                'credentials' => ['username' => 'rabbit_rs', 'password' => 'rabbit_rs_lab'],
                'tls' => ['enabled' => false, 'server_name' => null],
                'heartbeat' => 30,
            ],
        ],
        'routes' => [
            'default' => ['broker' => 'default', 'exchange' => '', 'routing_key' => '{queue}'],
        ],
        'workers' => [
            'default' => [
                'scheduler' => ['strategy' => 'weighted_fair'],
                'subscriptions' => [
                    'orders' => [
                        'enabled' => true,
                        'broker' => 'default',
                        'queue' => 'orders-eu',
                        'weight' => 1,
                        'priority_class' => 0,
                        'prefetch' => ['mode' => 'fixed', 'value' => 16],
                        'starvation_after' => 30,
                    ],
                ],
            ],
        ],
        'publisher' => ['confirms' => true, 'mandatory' => true],
        'topology' => [
            'queue' => ['type' => 'quorum', 'durable' => true, 'delivery_limit' => null],
            'dead_letter' => null,
        ],
    ];
}

describe('auto_subscribe pop', function () {
    it('pops a plain queue by auto-subscribing when enabled', function (): void {
        [$queue, $pool] = makeAutoSubscribeQueue(true);
        $pool->pushDelivery('__auto__.emails', new Delivery(
            '{"job":"ProcessEmail","data":{"to":"dev@example.com"}}',
            ['message_id' => 'auto-1', 'subscription' => 'auto', 'attempts' => 1],
        ));

        $job = $queue->pop('emails');

        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getQueue())->toBe('emails')
            ->and(['__auto__.emails'])->toBe($pool->consumerProfiles);
    });

    it('reuses the implicit profile on subsequent pops of the same queue', function (): void {
        [$queue, $pool] = makeAutoSubscribeQueue(true);

        $queue->pop('emails');
        $queue->pop('emails');

        expect($pool->consumerProfiles)->toBe(['__auto__.emails']);
    });

    it('keeps a worker profile name working on pop', function (): void {
        [$queue, $pool] = makeAutoSubscribeQueue(false);

        $queue->pop('default');

        expect(['default'])->toBe($pool->consumerProfiles);
    });

    it('rejects a plain queue without auto_subscribe', function (): void {
        [$queue] = makeAutoSubscribeQueue(false);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage(
            "No worker profile subscribes to queue 'emails'. "
            .'Configure workers.*.subscriptions.*.queue=emails or enable auto_subscribe.'
        );

        $queue->pop('emails');
    });
});

describe('auto_subscribe connector wiring', function () {
    it('honors auto_subscribe set at the connection level', function (): void {
        $pool = autoSubscribeSeededPool();

        $queue = autoSubscribeConnector($pool)->connect([
            'queue' => 'default',
            'auto_subscribe' => true,
        ]);
        $queue->setContainer(new Container());
        $queue->setConnectionName('rabbit-rs');
        $pool->pushDelivery('__auto__.emails', new Delivery(
            '{"job":"ProcessEmail","data":{}}',
            ['message_id' => 'auto-2', 'subscription' => 'auto', 'attempts' => 1],
        ));

        expect($queue->pop('emails'))->toBeInstanceOf(RabbitMqJob::class)
            ->and($queue->pop('emails'))->toBeNull();
    });

    it('honors auto_subscribe configured at the package level', function (): void {
        $this->app['config']->set('rabbit-rs.auto_subscribe', true);
        $pool = autoSubscribeSeededPool();

        $queue = autoSubscribeConnector($pool)->connect(['queue' => 'default']);
        $queue->setContainer(new Container());
        $queue->setConnectionName('rabbit-rs');
        $pool->pushDelivery('__auto__.emails', new Delivery(
            '{"job":"ProcessEmail","data":{"to":"dev@example.com"}}',
            ['message_id' => 'auto-3', 'subscription' => 'auto', 'attempts' => 1],
        ));

        expect($queue->pop('emails'))->toBeInstanceOf(RabbitMqJob::class)
            ->and($queue->pop('emails'))->toBeNull();
    });

    it('lets the connection config disable the package-level auto_subscribe', function (): void {
        $this->app['config']->set('rabbit-rs.auto_subscribe', true);
        $pool = new Pool(['workers' => autoSubscribeWorkers()]);

        $queue = autoSubscribeConnector($pool)->connect([
            'queue' => 'default',
            'auto_subscribe' => false,
        ]);
        $queue->setContainer(new Container());

        expect(fn () => $queue->pop('emails'))
            ->toThrow(InvalidArgumentException::class, 'enable auto_subscribe');
    });

    it('rejects a non-boolean connection auto_subscribe', function (): void {
        $pool = new Pool(['workers' => autoSubscribeWorkers()]);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('auto_subscribe must be a boolean');

        autoSubscribeConnector($pool)->connect(['queue' => 'default', 'auto_subscribe' => 'yes']);
    });
});

describe('auto_subscribe config normalization', function () {
    it('normalizes auto_subscribe to false by default', function (): void {
        expect(ConfigNormalizer::normalize(validAutoSubscribeConfig())['auto_subscribe'])->toBeFalse();
    });

    it('normalizes an enabled auto_subscribe', function (): void {
        $config = validAutoSubscribeConfig();
        $config['auto_subscribe'] = true;

        expect(ConfigNormalizer::normalize($config)['auto_subscribe'])->toBeTrue();
    });

    it('rejects a non-boolean auto_subscribe', function (): void {
        $config = validAutoSubscribeConfig();
        $config['auto_subscribe'] = 'yes';

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('auto_subscribe');

        ConfigNormalizer::normalize($config);
    });
});
