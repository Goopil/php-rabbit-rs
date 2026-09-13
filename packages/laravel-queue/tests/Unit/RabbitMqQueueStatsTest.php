<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;

/**
 * @return list<array<string, mixed>>
 */
function statsWorkers(): array
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
function statsRoutes(): array
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
function makeStatsQueue(string $defaultQueue = 'default'): array
{
    $pool = new Pool(['workers' => statsWorkers()]);
    $resolver = new WorkerProfileResolver(statsWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        statsRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
    );
    $queue->setContainer(new Container);

    return [$queue, $pool];
}

it('exposes the native pool stats including the return and drop counters', function (): void {
    [$queue] = makeStatsQueue();

    $stats = $queue->stats();

    expect($stats)->toBeArray()
        ->and($stats['returns_total'])->toBeInt()
        ->and($stats['dropped_publications_total'])->toBeInt()
        ->and($stats['deliveries_total'])->toBeInt();
});
