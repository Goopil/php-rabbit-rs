<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Exception as NativeException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\ProbeStatefile;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;
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
    ?Illuminate\Contracts\Container\Container $container = null,
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
    $queue->setContainer($container ?? new Container);
    $queue->setConnectionName('rabbit-main');

    return [$queue, $pool];
}

it('resolves queue name to worker profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('orders-eu');

    expect(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
});

it('resolves a different queue to a different profile on pop', function (): void {
    [$queue, $pool] = makePopQueue();

    $queue->pop('urgent-eu');

    expect(['high-priority'])->toBe($pool->consumerProfiles);
});

it('rejects an unknown queue on pop', function (): void {
    [$queue] = makePopQueue();

    $this->expectException(InvalidArgumentException::class);
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

    expect(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
});

it('rejects an unmarshable delivery toward the dead-letter exchange and returns null on pop', function (): void {
    Log::shouldReceive('error')->once();
    [$queue, $pool] = makePopQueue(hasDeadLetter: true, container: $this->app);

    $delivery = new Delivery('not-json', [
        'message_id' => '018f8f1a-unmarshable',
        'subscription' => 'auto',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('__auto__.orders-eu', $delivery);

    expect($queue->pop('orders-eu'))->toBeNull()
        ->and($delivery->rejectRequeues)->toBe([false])
        ->and($delivery->ackCalls)->toBe(0);
});

it('acknowledges an unmarshable delivery with a loud log when no dead-letter exchange is configured', function (): void {
    Log::shouldReceive('error')->once();
    [$queue, $pool] = makePopQueue(container: $this->app);

    $delivery = new Delivery('not-json', [
        'message_id' => '018f8f1a-unmarshable',
        'subscription' => 'auto',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('__auto__.orders-eu', $delivery);

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
        'subscription' => 'auto',
        'attempts' => 1,
        'state' => 'pending',
    ]);
    $pool->pushDelivery('__auto__.orders-eu', $delivery);

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
    $pool->consumerFor('__auto__.orders-eu')->throwOnNext(
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
    $pool->consumerFor('__auto__.orders-eu')->throwOnNext(
        new NativeException('consumer is closed'),
    );
    expect(fn () => $queue->pop('orders-eu'))->toThrow(QueueException::class);

    $queue->pop('orders-eu');
    expect($pool->consumerProfiles)->toHaveCount(2);
});

/**
 * Binds a probe statefile writer with a fixed pid so statefile assertions
 * are deterministic.
 */
function withProbeContainer(string $dir, int $pid = 777): Illuminate\Contracts\Container\Container
{
    $container = new Container;
    $container->instance(ProbeStatefile::class, new ProbeStatefile($dir, $pid));

    return $container;
}

/**
 * @return array<string, mixed>
 */
function readProbeState(string $dir, int $pid = 777): array
{
    return json_decode((string) file_get_contents($dir.'/'.$pid.'.json'), true) ?? [];
}

describe('probe statefile heartbeat', function () {
    it('writes a running statefile with pool stats counters after a pop', function (): void {
        $dir = probeTempDir();
        [$queue, $pool] = makePopQueue(container: withProbeContainer($dir));
        $pool->statsResult = ['deliveries_total' => 50, 'acks_total' => 48, 'rejects_total' => 2];

        $queue->pop('orders-eu');

        expect(readProbeState($dir))->toMatchArray([
            'pid' => 777,
            'state' => 'running',
            'connected' => true,
            'consumed' => 50,
            'acked' => 48,
            'nacked' => 2,
        ]);
    });

    it('skips the pool stats fetch between heartbeat windows', function (): void {
        $dir = probeTempDir();
        [$queue, $pool] = makePopQueue(container: withProbeContainer($dir));

        $queue->pop('orders-eu');
        expect($pool->statsCalls)->toBe(1);

        $queue->pop('orders-eu');
        expect($pool->statsCalls)->toBe(1);
    });

    it('records connection state transitions into the statefile', function (): void {
        $dir = probeTempDir();
        [$queue, $pool] = makePopQueue(container: withProbeContainer($dir));

        $queue->pop('orders-eu');
        $pool->simulateConnectionState('default-broker', 'recovering', 1);

        expect(readProbeState($dir)['connected'])->toBeFalse();
    });

    it('does not write a statefile when no writer is bound', function (): void {
        [$queue] = makePopQueue();

        expect($queue->pop('orders-eu'))->toBeNull();
    });
});
