<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;
use InvalidArgumentException;

final class MultiVhostWorkerTest extends TestCase
{
    public function testOneWorkerProfileConsumesDeliveriesFromThreeSubscriptionsAcrossTwoVhosts(): void
    {
        [$queue, $pool, $normalized] = $this->queue(blockFor: 2);
        $pool->pushDelivery('main', $this->delivery('orders_high', 2));
        $pool->pushDelivery('main', $this->delivery('orders_low', 4));
        $pool->pushDelivery('main', $this->delivery('billing', 6));

        $jobs = [$queue->pop(), $queue->pop(), $queue->pop()];

        self::assertSame([
            'billing_us' => '/billing-us',
            'orders_eu' => '/orders-eu',
        ], array_column($normalized['native']['brokers'], 'vhost', 'name'));
        self::assertSame(
            ['billing', 'orders_high', 'orders_low'],
            array_column($normalized['native']['workers'][0]['subscriptions'], 'name'),
        );
        self::assertSame(['main'], $pool->consumerProfiles);
        self::assertSame([2_000, 2_000, 2_000], $pool->consumerFor('main')->timeouts);
        self::assertSame(
            ['orders.high', 'orders.low', 'billing.invoices'],
            array_map(static fn ($job): string => $job->getQueue(), $jobs),
        );
        self::assertSame(
            ['rabbit-main', 'rabbit-main', 'rabbit-main'],
            array_map(static fn ($job): string => $job->getConnectionName(), $jobs),
        );
        self::assertSame(
            [2, 4, 6],
            array_map(static fn ($job): int => $job->attempts(), $jobs),
        );
    }

    public function testDisabledSubscriptionIsExcludedBeforeCreatingTheNativePool(): void
    {
        [, $pool, $normalized] = $this->queue();

        self::assertSame(
            ['billing', 'orders_high', 'orders_low'],
            array_column($normalized['native']['workers'][0]['subscriptions'], 'name'),
        );
        self::assertSame($normalized['native'], $pool->config);
    }

    public function testPublishedConfigurationEnablesItsDefaultSubscriptionExplicitly(): void
    {
        self::assertTrue(
            $this->app['config']->get('rabbit-rs.workers.default.subscriptions.default.enabled'),
        );
    }

    public function testUnknownProfileIsRejectedBeforeCallingTheNativePool(): void
    {
        [$queue, $pool] = $this->queue();

        try {
            $queue->pop('missing');
            self::fail('An unknown worker profile was accepted.');
        } catch (InvalidArgumentException $exception) {
            self::assertStringContainsString('missing', $exception->getMessage());
        }

        self::assertSame([], $pool->consumerProfiles);
    }

    public function testTimeoutWithoutDeliveryReturnsNull(): void
    {
        [$queue, $pool] = $this->queue(blockFor: 3);

        self::assertNull($queue->pop());
        self::assertSame([3_000], $pool->consumerFor('main')->timeouts);
    }

    public function testUnexpectedDeliverySubscriptionIsRejected(): void
    {
        [$queue, $pool] = $this->queue();
        $pool->pushDelivery('main', $this->delivery('disabled_legacy', 1));

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions.disabled_legacy');

        $queue->pop();
    }

    public function testNativeConsumerFailureBecomesAQueueException(): void
    {
        [$queue, $pool] = $this->queue();
        $native = new NativeException('consumer profile closed unexpectedly');
        $pool->consumerFor('main')->throwOnNext($native);

        try {
            $queue->pop();
            self::fail('The native consumer failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
        }
    }

    public function testNativeConnectionFailureRemainsRecognizable(): void
    {
        [$queue, $pool] = $this->queue();
        $native = new ConnectionException('consumer connection was lost');
        $pool->consumerFor('main')->throwOnNext($native);

        try {
            $queue->pop();
            self::fail('The native connection failure was not preserved.');
        } catch (ConnectionException $exception) {
            self::assertSame($native, $exception);
        }
    }

    public function testSubscriptionEnabledFlagMustBeBoolean(): void
    {
        $config = $this->config();
        $config['workers']['main']['subscriptions']['disabled_legacy']['enabled'] = 'false';

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions.disabled_legacy.enabled');

        ConfigNormalizer::normalize($config);
    }

    public function testWorkerMustKeepAtLeastOneEnabledSubscription(): void
    {
        $config = $this->config();
        foreach ($config['workers']['main']['subscriptions'] as &$subscription) {
            $subscription['enabled'] = false;
        }
        unset($subscription);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions');

        ConfigNormalizer::normalize($config);
    }

    public function testBlockForMustBeANonNegativeInteger(): void
    {
        $normalized = ConfigNormalizer::normalize($this->config());
        $connector = new RabbitMqConnector(new NativePoolFactory(), $normalized);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('block_for');

        $connector->connect(['queue' => 'main', 'block_for' => -1]);
    }

    /**
     * @return array{RabbitMqQueue, Pool, array<string, mixed>}
     */
    private function queue(int $blockFor = 0): array
    {
        $normalized = ConfigNormalizer::normalize($this->config());
        $pool = new Pool($normalized['native']);
        $connector = new RabbitMqConnector(
            new NativePoolFactory(createPool: static fn (array $config): Pool => $pool),
            $normalized,
        );
        $queue = $connector->connect([
            'queue' => 'main',
            'block_for' => $blockFor,
        ]);
        $queue->setContainer($this->app);
        $queue->setConnectionName('rabbit-main');

        return [$queue, $pool, $normalized];
    }

    private function delivery(string $subscription, int $attempts): Delivery
    {
        return new Delivery(
            '{"uuid":"018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137","job":"App\\Jobs\\Report"}',
            [
                'message_id' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
                'subscription' => $subscription,
                'attempts' => $attempts,
                'state' => 'pending',
                'headers' => [],
            ],
        );
    }

    /**
     * @return array<string, mixed>
     */
    private function config(): array
    {
        $credentials = [
            'username' => 'worker',
            'password' => 'secret',
        ];

        return [
            'topology_mode' => 'external',
            'brokers' => [
                'orders_eu' => [
                    'hosts' => ['orders-rabbit:5672'],
                    'vhost' => '/orders-eu',
                    'credentials' => $credentials,
                    'tls' => ['enabled' => false, 'server_name' => null],
                    'heartbeat' => 30,
                ],
                'billing_us' => [
                    'hosts' => ['billing-rabbit:5672'],
                    'vhost' => '/billing-us',
                    'credentials' => $credentials,
                    'tls' => ['enabled' => false, 'server_name' => null],
                    'heartbeat' => 30,
                ],
            ],
            'routes' => [
                'default' => [
                    'broker' => 'orders_eu',
                    'exchange' => 'laravel.jobs',
                    'routing_key' => '{queue}',
                ],
            ],
            'workers' => [
                'main' => [
                    'scheduler' => [
                        'strategy' => 'weighted_fair',
                        'max_in_flight' => 32,
                    ],
                    'subscriptions' => [
                        'orders_high' => [
                            'enabled' => true,
                            'broker' => 'orders_eu',
                            'queue' => 'orders.high',
                            'weight' => 8,
                            'priority_class' => 1,
                            'prefetch' => ['mode' => 'fixed', 'value' => 8],
                            'starvation_after' => 30,
                        ],
                        'orders_low' => [
                            'enabled' => true,
                            'broker' => 'orders_eu',
                            'queue' => 'orders.low',
                            'weight' => 2,
                            'priority_class' => 0,
                            'prefetch' => ['mode' => 'fixed', 'value' => 8],
                            'starvation_after' => 30,
                        ],
                        'billing' => [
                            'enabled' => true,
                            'broker' => 'billing_us',
                            'queue' => 'billing.invoices',
                            'weight' => 4,
                            'priority_class' => 0,
                            'prefetch' => ['mode' => 'fixed', 'value' => 8],
                            'starvation_after' => 30,
                        ],
                        'disabled_legacy' => [
                            'enabled' => false,
                            'broker' => 'billing_us',
                            'queue' => 'billing.legacy',
                            'weight' => 1,
                            'priority_class' => 0,
                            'prefetch' => ['mode' => 'fixed', 'value' => 8],
                            'starvation_after' => 30,
                        ],
                    ],
                ],
            ],
            'publisher' => ['confirms' => true, 'mandatory' => true],
            'topology' => [
                'queue' => [
                    'type' => 'quorum',
                    'durable' => true,
                    'delivery_limit' => 20,
                ],
                'dead_letter' => null,
            ],
        ];
    }
}