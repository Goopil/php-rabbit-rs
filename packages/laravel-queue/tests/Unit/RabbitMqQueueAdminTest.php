<?php

declare(strict_types=1);

use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Pool;

/**
 * @return array<string, array<string, string>>
 */
function adminRoutes(): array
{
    return [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'default.jobs',
            'routing_key' => '{queue}',
        ],
        'orders' => [
            'broker' => 'orders-broker',
            'exchange' => 'orders.jobs',
            'routing_key' => '{queue}.created.{queue}',
        ],
    ];
}

/**
 * @param array<string, array<string, string>> $routes
 */
function newAdminQueue(
    Pool $pool,
    array $routes,
    string $defaultQueue,
): RabbitMqQueue {
    $queue = new RabbitMqQueue($pool, $routes, $defaultQueue);
    $queue->setContainer(test()->app);

    return $queue;
}

/**
 * @return array{RabbitMqQueue, Pool}
 */
function adminQueue(): array
{
    $pool = new Pool([]);

    return [newAdminQueue($pool, adminRoutes(), 'default'), $pool];
}

describe('size', function (): void {
    it('returns the pending message count for the default queue', function (): void {
        [$queue, $pool] = adminQueue();
        $pool->sizeResults['default-broker:default'] = 42;

        expect(42)->toBe($queue->size());
    });

    it('resolves the route and queries the right broker', function (): void {
        [$queue, $pool] = adminQueue();
        $pool->sizeResults['orders-broker:orders'] = 7;

        expect(7)->toBe($queue->size('orders'))
            ->and([
                ['broker' => 'orders-broker', 'queue' => 'orders'],
            ])->toBe($pool->sizeCalls);
    });

    it('returns zero when no messages are pending', function (): void {
        [$queue, $pool] = adminQueue();

        expect(0)->toBe($queue->size())
            ->and($pool->sizeCalls)->toHaveCount(1);
    });

    it('translates a native failure into a QueueException', function (): void {
        [$queue, $pool] = adminQueue();
        $native = new NativeException('broker unreachable');
        $pool->throwOnNextSize($native);

        try {
            $queue->size();
            self::fail('The native size failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
            self::assertStringContainsString('broker unreachable', $exception->getMessage());
        }
    });

    it('fails when no route is configured', function (): void {
        $queue = newAdminQueue(new Pool([]), [
            'orders' => adminRoutes()['orders'],
        ], 'missing');

        $this->expectException(\InvalidArgumentException::class);
        $this->expectExceptionMessage('routes.missing');

        $queue->size();
    });
});

describe('clear', function (): void {
    it('purges the default queue', function (): void {
        [$queue, $pool] = adminQueue();

        $queue->clear();

        expect([
            ['broker' => 'default-broker', 'queue' => 'default'],
        ])->toBe($pool->clearCalls);
    });

    it('resolves the route and purges the right broker', function (): void {
        [$queue, $pool] = adminQueue();

        $queue->clear('orders');

        expect([
            ['broker' => 'orders-broker', 'queue' => 'orders'],
        ])->toBe($pool->clearCalls);
    });

    it('translates a native failure into a QueueException', function (): void {
        [$queue, $pool] = adminQueue();
        $native = new NativeException('purge refused');
        $pool->throwOnNextClear($native);

        try {
            $queue->clear();
            self::fail('The native clear failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
            self::assertStringContainsString('purge refused', $exception->getMessage());
        }
    });
});
