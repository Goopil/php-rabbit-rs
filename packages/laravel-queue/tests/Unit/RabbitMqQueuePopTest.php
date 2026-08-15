<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use PHPUnit\Framework\TestCase;

final class RabbitMqQueuePopTest extends TestCase
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

    public function testPopResolvesQueueNameToWorkerProfile(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('orders-eu');

        self::assertSame(['default'], $pool->consumerProfiles);
    }

    public function testPopResolvesDifferentQueueToDifferentProfile(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $queue->pop('urgent-eu');

        self::assertSame(['high-priority'], $pool->consumerProfiles);
    }

    public function testPopRejectsUnknownQueue(): void
    {
        [$queue, $pool] = $this->makeQueue();

        $this->expectException(\InvalidArgumentException::class);
        $this->expectExceptionMessage('No worker profile subscribes to queue');

        $queue->pop('unknown-queue');
    }

    public function testPopWithNullQueueUsesDefaultQueueAsProfile(): void
    {
        [$queue, $pool] = $this->makeQueue('default');

        $queue->pop();

        self::assertSame(['default'], $pool->consumerProfiles);
    }

    public function testPopWithNullQueueResolvesDefaultQueueToProfileWhenItIsAQueueName(): void
    {
        [$queue, $pool] = $this->makeQueue('orders-eu');

        $queue->pop();

        self::assertSame(['default'], $pool->consumerProfiles);
    }
}
