<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Log;

/**
 * @return list<array<string, mixed>>
 */
function popWorkers(): array
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
function popRoutes(): array
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
function makePopQueue(
    string $defaultQueue = 'default',
    bool $hasDeadLetter = false,
    ?\Illuminate\Contracts\Container\Container $container = null,
): array {
    $pool = new Pool(['workers' => popWorkers()]);
    $resolver = new WorkerProfileResolver(popWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        popRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
        hasDeadLetter: $hasDeadLetter,
    );
    $queue->setContainer($container ?? new \Illuminate\Container\Container());
    $queue->setConnectionName('rabbit-main');

    return [$queue, $pool];
}

it('resolves queue name to worker profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('orders-eu');

    expect(['default'])->toBe($pool->consumerProfiles);
});

it('resolves a different queue to a different profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('urgent-eu');

    expect(['high-priority'])->toBe($pool->consumerProfiles);
});

it('rejects an unknown queue on pop', function (): void {
    [$queue] = makePopQueue();

    $this->expectException(\InvalidArgumentException::class);
    $this->expectExceptionMessage('No worker profile subscribes to queue');

    $queue->pop('unknown-queue');
});

it('uses the default queue name as profile when pop is called with null', function (): void {
    [$queue, $pool] = makePopQueue('default');

    $queue->pop();

    expect(['default'])->toBe($pool->consumerProfiles);
});

it('resolves the default queue to its profile when it is a queue name and pop is called with null', function (): void {
    [$queue, $pool] = makePopQueue('orders-eu');

    $queue->pop();

    expect(['default'])->toBe($pool->consumerProfiles);
});

it('rejects an unmarshable delivery toward the dead-letter exchange and returns null on pop', function (): void {
    Log::shouldReceive('error')->once();
    [$queue, $pool] = makePopQueue(hasDeadLetter: true, container: $this->app);

    $delivery = new Delivery('not-json', [
        'message_id' => '018f8f1a-unmarshable',
        'subscription' => 'orders',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('default', $delivery);

    expect($queue->pop('orders-eu'))->toBeNull()
        ->and($delivery->rejectRequeues)->toBe([false])
        ->and($delivery->ackCalls)->toBe(0);
});

it('acknowledges an unmarshable delivery with a loud log when no dead-letter exchange is configured', function (): void {
    Log::shouldReceive('error')->once();
    [$queue, $pool] = makePopQueue(container: $this->app);

    $delivery = new Delivery('not-json', [
        'message_id' => '018f8f1a-unmarshable',
        'subscription' => 'orders',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('default', $delivery);

    expect($queue->pop('orders-eu'))->toBeNull()
        ->and($delivery->ackCalls)->toBe(1)
        ->and($delivery->rejectRequeues)->toBe([]);
});

it('does not settle a marshable delivery on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $delivery = new Delivery(json_encode([
        'uuid' => '018f8f1a-marshable',
        'job' => 'stdClass',
        'data' => [],
    ], JSON_THROW_ON_ERROR), [
        'message_id' => '018f8f1a-marshable',
        'subscription' => 'orders',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('default', $delivery);

    $job = $queue->pop('orders-eu');

    expect($job)->not->toBeNull()
        ->and($delivery->ackCalls)->toBe(0)
        ->and($delivery->rejectRequeues)->toBe([]);
});

it('evicts the cached consumer so the next pop re-fetches after a connection error', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('orders-eu');
    expect($pool->consumerProfiles)->toHaveCount(1);

    // A connection-level error carries SourceReplaced ("re-fetch consumer"),
    // StaleGeneration and Transport: the retired handle must not be reused.
    $pool->consumerFor('default')->throwOnNext(
        new ConnectionException('broker source replaced by recovery; re-fetch consumer'),
    );
    expect(fn () => $queue->pop('orders-eu'))->toThrow(ConnectionException::class);

    // The next pop must re-fetch from the pool instead of reusing the
    // retired handle the one-shot signal was delivered to.
    $queue->pop('orders-eu');
    expect($pool->consumerProfiles)->toHaveCount(2);
});

it('evicts the cached consumer so the next pop re-fetches after the consumer closed', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('orders-eu');
    expect($pool->consumerProfiles)->toHaveCount(1);

    // The Closed kind surfaces as the base native exception and is wrapped
    // in QueueException: every source retired, the handle is terminal.
    $pool->consumerFor('default')->throwOnNext(
        new NativeException('consumer is closed'),
    );
    expect(fn () => $queue->pop('orders-eu'))->toThrow(QueueException::class);

    $queue->pop('orders-eu');
    expect($pool->consumerProfiles)->toHaveCount(2);
});
