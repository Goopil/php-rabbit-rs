<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;

/**
 * @return list<array<string, mixed>>
 */
function cleanupWorkers(): array
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
function cleanupRoutes(): array
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
function makeCleanupQueue(string $defaultQueue = 'default'): array
{
    $pool = new Pool(['workers' => cleanupWorkers()]);
    $resolver = new WorkerProfileResolver(cleanupWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        cleanupRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
    );
    $queue->setContainer(new \Illuminate\Container\Container());

    return [$queue, $pool];
}

describe('closeConsumers', function (): void {
    it('closes all cached consumers', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        // Create two consumers by popping from two different profiles.
        $queue->pop('orders-eu');
        $queue->pop('urgent-eu');

        $consumer1 = $pool->consumerFor('default');
        $consumer2 = $pool->consumerFor('high-priority');

        expect(0)->toBe($consumer1->closeCalls)
            ->and(0)->toBe($consumer2->closeCalls);

        $queue->closeConsumers();

        expect(1)->toBe($consumer1->closeCalls)
            ->and(1)->toBe($consumer2->closeCalls);
    });

    it('clears the consumer cache', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        expect(['default'])->toBe($pool->consumerProfiles);

        $queue->closeConsumers();

        // After closeConsumers, calling pop again must create a new consumer.
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        expect(['default'])->toBe($pool->consumerProfiles);
    });

    it('is idempotent', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('default');

        $queue->closeConsumers();
        $queue->closeConsumers();

        expect(1)->toBe($consumer->closeCalls);
    });

    it('is safe with no cached consumers', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        // Should not throw.
        $queue->closeConsumers();

        expect(true)->toBeTrue();
    });

    it('creates a new consumer on pop after closeConsumers', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $firstConsumer = $pool->consumerFor('default');

        $queue->closeConsumers();
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        $secondConsumer = $pool->consumerFor('default');

        expect($firstConsumer)->not->toBe($secondConsumer);
    });
});

describe('destruct', function (): void {
    it('calls closeConsumers on destruct', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('default');

        expect(0)->toBe($consumer->closeCalls);

        unset($queue);

        expect(1)->toBe($consumer->closeCalls);
    });
});
