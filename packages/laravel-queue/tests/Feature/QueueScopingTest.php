<?php

declare(strict_types=1);

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;

/**
 * @return list<array<string, mixed>>
 */
function scopingSharedWorkers(): array
{
    return [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'orders', 'queue' => 'orders-eu'],
                ['name' => 'billing', 'queue' => 'billing-eu'],
            ],
        ],
    ];
}

/**
 * A pool seeded with the implicit profile that scoping requests for
 * 'orders-eu', mirroring the core synthesizing `__auto__.` profiles at
 * first pop.
 */
function scopingSeededPool(): Pool
{
    return new Pool(['workers' => [...scopingSharedWorkers(), [
        'name' => '__auto__.orders-eu',
        'subscriptions' => [
            ['name' => 'auto', 'queue' => 'orders-eu'],
        ],
    ]]]);
}

/**
 * @return array{RabbitMqQueue, Pool}
 */
function makeScopingQueue(bool $autoSubscribe): array
{
    $pool = scopingSeededPool();
    $queue = new RabbitMqQueue(
        $pool,
        ['default' => ['broker' => 'default-broker', 'exchange' => '', 'routing_key' => '{queue}']],
        'orders-eu',
        autoSubscribe: $autoSubscribe,
        workerProfiles: new WorkerProfileResolver(scopingSharedWorkers()),
    );
    $queue->setContainer(new Container);
    $queue->setConnectionName('rabbit-rs');

    return [$queue, $pool];
}

/**
 * @return list<array<string, mixed>>
 */
function scopingAlphaBetaWorkers(): array
{
    return [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'alpha-sub', 'queue' => 'alpha'],
                ['name' => 'beta-sub', 'queue' => 'beta'],
            ],
        ],
    ];
}

/**
 * Pool seeded with the implicit profiles scoping resolves for 'alpha' and
 * 'beta', mirroring the core synthesizing `__auto__.` profiles at first pop.
 */
function scopingAlphaBetaPool(): Pool
{
    return new Pool(['workers' => [...scopingAlphaBetaWorkers(), [
        'name' => '__auto__.alpha',
        'subscriptions' => [
            ['name' => 'auto', 'queue' => 'alpha'],
        ],
    ], [
        'name' => '__auto__.beta',
        'subscriptions' => [
            ['name' => 'auto', 'queue' => 'beta'],
        ],
    ]]]);
}

/**
 * @return array{RabbitMqQueue, Pool}
 */
function makeAlphaBetaQueue(): array
{
    $pool = scopingAlphaBetaPool();
    $queue = new RabbitMqQueue(
        $pool,
        ['default' => ['broker' => 'default-broker', 'exchange' => '', 'routing_key' => '{queue}']],
        'alpha',
        autoSubscribe: false,
        workerProfiles: new WorkerProfileResolver(scopingAlphaBetaWorkers()),
    );
    $queue->setContainer(new Container);
    $queue->setConnectionName('rabbit-rs');

    return [$queue, $pool];
}

describe('queue scoping on shared profiles', function () {
    it('pops a single queue through a scoped profile instead of the shared one', function (): void {
        [$queue, $pool] = makeScopingQueue(true);
        $pool->pushDelivery('__auto__.orders-eu', new Delivery(
            '{"job":"ProcessOrder","data":{}}',
            ['message_id' => 'scoped-1', 'subscription' => 'auto', 'attempts' => 1],
        ));

        $job = $queue->pop('orders-eu');

        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getQueue())->toBe('orders-eu')
            ->and(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
    });

    it('scopes pop(null) to the default queue of a shared profile', function (): void {
        [$queue, $pool] = makeScopingQueue(true);
        $pool->pushDelivery('__auto__.orders-eu', new Delivery(
            '{"job":"ProcessOrder","data":{}}',
            ['message_id' => 'scoped-2', 'subscription' => 'auto', 'attempts' => 1],
        ));

        $job = $queue->pop();

        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getQueue())->toBe('orders-eu')
            ->and(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
    });

    it('keeps the compiled profile for a single-queue connection', function (): void {
        $pool = new Pool(['workers' => [[
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'orders', 'queue' => 'orders-eu'],
            ],
        ]]]);
        $queue = new RabbitMqQueue(
            $pool,
            ['default' => ['broker' => 'default-broker', 'exchange' => '', 'routing_key' => '{queue}']],
            'orders-eu',
            autoSubscribe: true,
            workerProfiles: new WorkerProfileResolver([[
                'name' => 'default',
                'subscriptions' => [
                    ['name' => 'orders', 'queue' => 'orders-eu'],
                ],
            ]]),
        );
        $queue->setContainer(new Container);

        $queue->pop('orders-eu');

        expect(['default'])->toBe($pool->consumerProfiles);
    });

    it('scopes a shared profile even when auto_subscribe is disabled', function (): void {
        [$queue, $pool] = makeScopingQueue(false);
        $pool->pushDelivery('__auto__.orders-eu', new Delivery(
            '{"job":"ProcessOrder","data":{}}',
            ['message_id' => 'scoped-4', 'subscription' => 'auto', 'attempts' => 1],
        ));

        $job = $queue->pop('orders-eu');

        expect($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getQueue())->toBe('orders-eu')
            ->and(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
    });

    it('never draws a beta job through pop(alpha) on a default multi-queue connection', function (): void {
        [$queue, $pool] = makeAlphaBetaQueue();
        $pool->pushDelivery('__auto__.beta', new Delivery(
            '{"job":"ProcessBeta","data":{}}',
            ['message_id' => 'scoped-3', 'subscription' => 'auto', 'attempts' => 1],
        ));

        $job = $queue->pop('beta');

        expect($queue->pop('alpha'))->toBeNull()
            ->and($job)->toBeInstanceOf(RabbitMqJob::class)
            ->and($job->getQueue())->toBe('beta')
            ->and($pool->consumerProfiles)->toBe(['__auto__.beta', '__auto__.alpha']);
    });

    it('pops a shared profile by name without scoping', function (): void {
        [$queue, $pool] = makeScopingQueue(true);

        $queue->pop('default');

        expect(['default'])->toBe($pool->consumerProfiles);
    });
});
