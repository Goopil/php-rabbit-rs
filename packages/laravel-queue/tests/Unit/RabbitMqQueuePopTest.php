<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;

/**
 * @return list<array<string, mixed>>
 */
function popWorkers(): array
{
    return [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'orders', 'queue' => 'orders-eu'],
                ['name' => 'billing', 'queue' => 'billing-eu'],
            ],
        ],
        [
            'name' => 'high-priority',
            'subscriptions' => [
                ['name' => 'urgent', 'queue' => 'urgent-eu'],
            ],
        ],
    ];
}

/**
 * @return array<string, array<string, string>>
 */
function popRoutes(): array
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
 * @return array{RabbitMqQueue, Pool}
 */
function makePopQueue(string $defaultQueue = 'default'): array
{
    $pool = new Pool(['workers' => popWorkers()]);
    $resolver = new WorkerProfileResolver(popWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        popRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
    );
    $queue->setContainer(new \Illuminate\Container\Container());

    return [$queue, $pool];
}

it('resolves queue name to worker profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('orders-eu');

    expect(['default'])->toBe($pool->consumerProfiles);
});

it('resolves a different queue to a different profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('urgent-eu');

    expect(['high-priority'])->toBe($pool->consumerProfiles);
});

it('rejects an unknown queue on pop', function (): void {
    [$queue] = makePopQueue();

    $this->expectException(\InvalidArgumentException::class);
    $this->expectExceptionMessage('No worker profile subscribes to queue');

    $queue->pop('unknown-queue');
});

it('uses the default queue name as profile when pop is called with null', function (): void {
    [$queue, $pool] = makePopQueue('default');

    $queue->pop();

    expect(['default'])->toBe($pool->consumerProfiles);
});

it('resolves the default queue to its profile when it is a queue name and pop is called with null', function (): void {
    [$queue, $pool] = makePopQueue('orders-eu');

    $queue->pop();

    expect(['default'])->toBe($pool->consumerProfiles);
});
