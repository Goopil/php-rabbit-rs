<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;

final class RabbitMqQueueAdminTest extends TestCase
{
    public function testSizeReturnsPendingMessageCountForDefaultQueue(): void
    {
        [$queue, $pool] = $this->queue();
        $pool->sizeResults['default-broker:default'] = 42;

        self::assertSame(42, $queue->size());
    }

    public function testSizeResolvesTheRouteAndQueriesTheRightBroker(): void
    {
        [$queue, $pool] = $this->queue();
        $pool->sizeResults['orders-broker:orders'] = 7;

        self::assertSame(7, $queue->size('orders'));
        self::assertSame([
            ['broker' => 'orders-broker', 'queue' => 'orders'],
        ], $pool->sizeCalls);
    }

    public function testSizeReturnsZeroWhenNoMessagesArePending(): void
    {
        [$queue, $pool] = $this->queue();

        self::assertSame(0, $queue->size());
        self::assertCount(1, $pool->sizeCalls);
    }

    public function testClearPurgesTheDefaultQueue(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->clear();

        self::assertSame([
            ['broker' => 'default-broker', 'queue' => 'default'],
        ], $pool->clearCalls);
    }

    public function testClearResolvesTheRouteAndPurgesTheRightBroker(): void
    {
        [$queue, $pool] = $this->queue();

        $queue->clear('orders');

        self::assertSame([
            ['broker' => 'orders-broker', 'queue' => 'orders'],
        ], $pool->clearCalls);
    }

    public function testNativeFailureOnSizeBecomesAQueueException(): void
    {
        [$queue, $pool] = $this->queue();
        $native = new NativeException('broker unreachable');
        $pool->throwOnNextSize($native);

        try {
            $queue->size();
            self::fail('The native size failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
            self::assertStringContainsString('broker unreachable', $exception->getMessage());
        }
    }

    public function testNativeFailureOnClearBecomesAQueueException(): void
    {
        [$queue, $pool] = $this->queue();
        $native = new NativeException('purge refused');
        $pool->throwOnNextClear($native);

        try {
            $queue->clear();
            self::fail('The native clear failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
            self::assertStringContainsString('purge refused', $exception->getMessage());
        }
    }

    public function testSizeFailsWhenNoRouteIsConfigured(): void
    {
        $queue = $this->newQueue(new Pool([]), [
            'orders' => $this->routes()['orders'],
        ], 'missing');

        $this->expectException(\InvalidArgumentException::class);
        $this->expectExceptionMessage('routes.missing');

        $queue->size();
    }

    /**
     * @return array{RabbitMqQueue, Pool}
     */
    private function queue(): array
    {
        $pool = new Pool([]);

        return [$this->newQueue($pool, $this->routes(), 'default'), $pool];
    }

    /**
     * @param array<string, array<string, string>> $routes
     */
    private function newQueue(
        Pool $pool,
        array $routes,
        string $defaultQueue,
    ): RabbitMqQueue {
        $queue = new RabbitMqQueue($pool, $routes, $defaultQueue);
        $queue->setContainer($this->app);

        return $queue;
    }

    /**
     * @return array<string, array<string, string>>
     */
    private function routes(): array
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
}
