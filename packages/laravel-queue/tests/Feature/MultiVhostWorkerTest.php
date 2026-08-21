<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

describe('multi-vhost worker', function () {
    it('one worker profile consumes deliveries from three subscriptions across two vhosts', function () {
        [$queue, $pool, $normalized] = multiVhostQueue($this->app, blockFor: 2);
        $pool->pushDelivery('main', multiVhostDelivery('orders_high', 2));
        $pool->pushDelivery('main', multiVhostDelivery('orders_low', 4));
        $pool->pushDelivery('main', multiVhostDelivery('billing', 6));

        $jobs = [$queue->pop(), $queue->pop(), $queue->pop()];

        expect(array_column($normalized['native']['brokers'], 'vhost', 'name'))->toBe([
            'billing_us' => '/billing-us',
            'orders_eu' => '/orders-eu',
        ]);
        expect(
            array_column($normalized['native']['workers'][0]['subscriptions'], 'name'),
        )->toBe(['billing', 'orders_high', 'orders_low']);
        expect($pool->consumerProfiles)->toBe(['main']);
        expect($pool->consumerFor('main')->timeouts)->toBe([2_000, 2_000, 2_000]);
        expect(
            array_map(static fn ($job): string => $job->getQueue(), $jobs),
        )->toBe(['orders.high', 'orders.low', 'billing.invoices']);
        expect(
            array_map(static fn ($job): string => $job->getConnectionName(), $jobs),
        )->toBe(['rabbit-main', 'rabbit-main', 'rabbit-main']);
        expect(
            array_map(static fn ($job): int => $job->attempts(), $jobs),
        )->toBe([2, 4, 6]);
    });

    it('disabled subscription is excluded before creating the native pool', function () {
        [, $pool, $normalized] = multiVhostQueue($this->app);

        expect(
            array_column($normalized['native']['workers'][0]['subscriptions'], 'name'),
        )->toBe(['billing', 'orders_high', 'orders_low']);
        expect($normalized['native'])->toBe($pool->config);
    });

    it('published configuration enables its default subscription explicitly', function () {
        expect(
            $this->app['config']->get('rabbit-rs.workers.default.subscriptions.default.enabled'),
        )->toBeTrue();
    });

    it('unknown profile is rejected before calling the native pool', function () {
        [$queue, $pool] = multiVhostQueue($this->app);

        try {
            $queue->pop('missing');
            self::fail('An unknown worker profile was accepted.');
        } catch (InvalidArgumentException $exception) {
            self::assertStringContainsString('missing', $exception->getMessage());
        }

        expect($pool->consumerProfiles)->toBe([]);
    });

    it('timeout without delivery returns null', function () {
        [$queue, $pool] = multiVhostQueue($this->app, blockFor: 3);

        expect($queue->pop())->toBeNull();
        expect($pool->consumerFor('main')->timeouts)->toBe([3_000]);
    });

    it('unexpected delivery subscription is rejected', function () {
        [$queue, $pool] = multiVhostQueue($this->app);
        $pool->pushDelivery('main', multiVhostDelivery('disabled_legacy', 1));

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions.disabled_legacy');

        $queue->pop();
    });

    it('native consumer failure becomes a queue exception', function () {
        [$queue, $pool] = multiVhostQueue($this->app);
        $native = new NativeException('consumer profile closed unexpectedly');
        $pool->consumerFor('main')->throwOnNext($native);

        try {
            $queue->pop();
            self::fail('The native consumer failure was not translated.');
        } catch (QueueException $exception) {
            self::assertSame($native, $exception->getPrevious());
        }
    });

    it('native connection failure remains recognizable', function () {
        [$queue, $pool] = multiVhostQueue($this->app);
        $native = new ConnectionException('consumer connection was lost');
        $pool->consumerFor('main')->throwOnNext($native);

        try {
            $queue->pop();
            self::fail('The native connection failure was not preserved.');
        } catch (ConnectionException $exception) {
            self::assertSame($native, $exception);
        }
    });

    it('subscription enabled flag must be boolean', function () {
        $config = multiVhostConfig();
        $config['workers']['main']['subscriptions']['disabled_legacy']['enabled'] = 'false';

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions.disabled_legacy.enabled');

        ConfigNormalizer::normalize($config);
    });

    it('worker must keep at least one enabled subscription', function () {
        $config = multiVhostConfig();
        foreach ($config['workers']['main']['subscriptions'] as &$subscription) {
            $subscription['enabled'] = false;
        }
        unset($subscription);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions');

        ConfigNormalizer::normalize($config);
    });

    it('block_for must be a non-negative integer', function () {
        $normalized = ConfigNormalizer::normalize(multiVhostConfig());
        $connector = new RabbitMqConnector(new NativePoolFactory(), $normalized);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('block_for');

        $connector->connect(['queue' => 'main', 'block_for' => -1]);
    });
});

/**
 * @return array{RabbitMqQueue, Pool, array<string, mixed>}
 */
function multiVhostQueue($app, int $blockFor = 0): array
{
    $normalized = ConfigNormalizer::normalize(multiVhostConfig());
    $pool = new Pool($normalized['native']);
    $connector = new RabbitMqConnector(
        new NativePoolFactory(createPool: static fn (array $config): Pool => $pool),
        $normalized,
    );
    $queue = $connector->connect([
        'queue' => 'main',
        'block_for' => $blockFor,
    ]);
    $queue->setContainer($app);
    $queue->setConnectionName('rabbit-main');

    return [$queue, $pool, $normalized];
}

function multiVhostDelivery(string $subscription, int $attempts): Delivery
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
function multiVhostConfig(): array
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
