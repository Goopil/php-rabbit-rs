<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;

/**
 * @return array{RabbitMqQueue, Pool}
 */
function makeDrainQueue(string $defaultQueue = 'default'): array
{
    $pool = new Pool(['workers' => popWorkers()]);
    $resolver = new WorkerProfileResolver(popWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        popRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
    );
    $queue->setContainer(app());

    return [$queue, $pool];
}

/**
 * Warm the queue's consumer cache so drainSettlementErrors can see it.
 *
 * The queue lazily caches consumers in its own $consumers array on the
 * first pop() call.  Calling pop() once with no deliveries populates the
 * cache without side effects.
 */
function warmConsumerCache(RabbitMqQueue $queue, string $queueName = 'orders-eu'): void
{
    $queue->pop($queueName);
}

it('drainSettlementErrors throws ConnectionException on StaleGeneration error', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'StaleGeneration',
        'message' => 'stale generation detected',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(ConnectionException::class);
});

it('drainSettlementErrors throws ConnectionException on Transport error', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'Transport',
        'message' => 'transport error',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(ConnectionException::class);
});

it('drainSettlementErrors does not throw on non-connection errors', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'AlreadySettled',
        'message' => 'delivery already settled',
    ]);

    $queue->drainSettlementErrors();

    expect(true)->toBeTrue();
});

it('drainSettlementErrors clears errors after draining', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'AlreadySettled',
        'message' => 'delivery already settled',
    ]);

    $queue->drainSettlementErrors();

    expect($consumer->drainErrors())->toBe([]);
});

it('drainSettlementErrors is a no-op when there are no errors', function (): void {
    [$queue] = makeDrainQueue();

    $queue->drainSettlementErrors();

    expect(true)->toBeTrue();
});

it('pop calls drainSettlementErrors before getting deliveries', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'StaleGeneration',
        'message' => 'stale generation on pop',
    ]);

    expect(fn () => $queue->pop('orders-eu'))
        ->toThrow(ConnectionException::class);
});
