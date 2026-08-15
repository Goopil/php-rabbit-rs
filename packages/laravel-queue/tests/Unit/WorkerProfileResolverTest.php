<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use PHPUnit\Framework\TestCase;

final class WorkerProfileResolverTest extends TestCase
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

    public function testProfileForQueueReturnsTheProfileThatSubscribesToTheQueue(): void
    {
        $resolver = new WorkerProfileResolver($this->workers());

        self::assertSame('default', $resolver->profileForQueue('orders-eu'));
        self::assertSame('default', $resolver->profileForQueue('billing-eu'));
        self::assertSame('high-priority', $resolver->profileForQueue('urgent-eu'));
    }

    public function testProfileForQueueReturnsNullWhenNoProfileSubscribesToTheQueue(): void
    {
        $resolver = new WorkerProfileResolver($this->workers());

        self::assertNull($resolver->profileForQueue('unknown-queue'));
    }

    public function testProfileForQueueReturnsFirstMatchWhenMultipleProfilesSubscribeToTheSameQueue(): void
    {
        $workers = [
            [
                'name' => 'first',
                'subscriptions' => [
                    ['name' => 'main', 'queue' => 'shared-queue'],
                ],
            ],
            [
                'name' => 'second',
                'subscriptions' => [
                    ['name' => 'backup', 'queue' => 'shared-queue'],
                ],
            ],
        ];
        $resolver = new WorkerProfileResolver($workers);

        self::assertSame('first', $resolver->profileForQueue('shared-queue'));
    }
}
