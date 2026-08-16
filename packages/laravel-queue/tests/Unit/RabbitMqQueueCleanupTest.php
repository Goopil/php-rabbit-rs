<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use PHPUnit\Framework\TestCase;

final class RabbitMqQueueCleanupTest extends TestCase
{
    /**
     * @return list<array<string, mixed>>
     */
    private function workers(): array
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
    private function routes(): array
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
    private function makeQueue(string $defaultQueue = 'default'): array
    {
        $pool = new Pool(['workers' => $this->workers()]);
        $resolver = new WorkerProfileResolver($this->workers());
        $queue = new RabbitMqQueue(
            $pool,
            $this->routes(),
            $defaultQueue,
            workerProfiles: $resolver,
        );
        $queue->setContainer(new \Illuminate\Container\Container());

        return [$queue, $pool];
    }

    public function testCloseConsumersClosesAllCachedConsumers(): void
    {
        [$queue, $pool] = $this->makeQueue();

        // Create two consumers by popping from two different profiles.
        $queue->pop('orders-eu');
        $queue->pop('urgent-eu');

        $consumer1 = $pool->consumerFor('default');
        $consumer2 = $pool->consumerFor('high-priority');

        self::assertSame(0, $consumer1->closeCalls);
        self::assertSame(0, $consumer2->closeCalls);

        $queue->closeConsumers();

        self::assertSame(1, $consumer1->closeCalls, 'first consumer must be closed');
        self::assertSame(1, $consumer2->closeCalls, 'second consumer must be closed');
    }

    public function testCloseConsumersClearsTheCache(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('orders-eu');
        self::assertSame(['default'], $pool->consumerProfiles);

        $queue->closeConsumers();

        // After closeConsumers, calling pop again must create a new consumer.
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        self::assertSame(['default'], $pool->consumerProfiles, 'pop must create a fresh consumer after closeConsumers');
    }

    public function testCloseConsumersIsIdempotent(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('default');

        $queue->closeConsumers();
        $queue->closeConsumers();

        self::assertSame(1, $consumer->closeCalls, 'close() must only be called once per consumer');
    }

    public function testCloseConsumersWithNoCachedConsumersIsSafe(): void
    {
        [$queue, $pool] = $this->makeQueue();

        // Should not throw.
        $queue->closeConsumers();

        self::assertTrue(true);
    }

    public function testDestructCallsCloseConsumers(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('default');

        self::assertSame(0, $consumer->closeCalls);

        unset($queue);

        self::assertSame(1, $consumer->closeCalls, '__destruct must close all cached consumers');
    }

    public function testPopAfterCloseConsumersCreatesNewConsumer(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('orders-eu');
        $firstConsumer = $pool->consumerFor('default');

        $queue->closeConsumers();
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        $secondConsumer = $pool->consumerFor('default');

        self::assertNotSame($firstConsumer, $secondConsumer, 'pop must return a new consumer after closeConsumers');
    }
}
