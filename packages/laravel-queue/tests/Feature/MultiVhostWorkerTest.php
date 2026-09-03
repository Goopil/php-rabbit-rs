<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

describe('multi-subscription worker', function () {
    it('one worker profile consumes deliveries from three subscriptions', function () {
        [$queue, $pool, $compiled] = multiVhostQueue($this->app, blockFor: 2);
        $pool->pushDelivery('main', multiVhostDelivery('orders_high', 2));
        $pool->pushDelivery('main', multiVhostDelivery('orders_low', 4));
        $pool->pushDelivery('main', multiVhostDelivery('billing', 6));

        $jobs = [$queue->pop(), $queue->pop(), $queue->pop()];

        expect($compiled['native']['brokers'])->toHaveCount(1)
            ->and($compiled['native']['brokers'][0]['name'])->toBe('main')
            ->and($compiled['native']['brokers'][0]['vhost'])->toBe('/orders-eu');
        expect(
            array_column($compiled['native']['workers'][0]['subscriptions'], 'name'),
        )->toBe(['orders_high', 'orders_low', 'billing']);
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

    it('compiles the native config the pool is built from', function () {
        [, $pool, $compiled] = multiVhostQueue($this->app);

        expect($compiled['native'])->toBe($pool->config);
        expect($compiled['native']['topology_mode'])->toBe('external');
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
        $pool->pushDelivery('main', multiVhostDelivery('ghost', 1));

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('workers.main.subscriptions.ghost');

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

    it('connection must keep at least one subscription', function () {
        $config = multiVhostConfig();
        $config['subscriptions'] = [];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('must contain at least one subscription');

        ConnectionCompiler::compile('main', $config);
    });

    it('block_for must be a non-negative integer', function () {
        $connector = new RabbitMqConnector(new NativePoolFactory());

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
    $config = multiVhostConfig() + ['block_for' => $blockFor];
    // Register the connection so the connector's reverse lookup names the
    // compiled broker and worker profile after it ('main').
    $app['config']->set('queue.connections.main', $config);

    $compiled = ConnectionCompiler::compile('main', $config);
    $pool = new Pool($compiled['native']);
    $connector = new RabbitMqConnector(
        new NativePoolFactory(createPool: static fn (array $config): Pool => $pool),
        is_array($app['config']->get('rabbit-rs')) ? $app['config']->get('rabbit-rs') : [],
    );
    $queue = $connector->connect($config);
    $queue->setContainer($app);
    $queue->setConnectionName('rabbit-main');

    return [$queue, $pool, $compiled];
}

function multiVhostDelivery(string $subscription, int $attempts): Delivery
{
    return new Delivery(
        json_encode([
            'uuid' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
            'job' => 'App\\Jobs\\Report',
        ], JSON_THROW_ON_ERROR),
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
    return [
        'queue' => 'main',
        'hosts' => 'orders-rabbit:5672',
        'vhost' => '/orders-eu',
        'username' => 'worker',
        'password' => 'secret',
        'topology_mode' => 'external',
        'subscriptions' => [
            'orders_high' => [
                'queue' => 'orders.high',
                'weight' => 8,
                'priority_class' => 1,
                'prefetch' => 8,
            ],
            'orders_low' => [
                'queue' => 'orders.low',
                'weight' => 2,
                'prefetch' => 8,
            ],
            'billing' => [
                'queue' => 'billing.invoices',
                'weight' => 4,
                'prefetch' => 8,
            ],
        ],
    ];
}
