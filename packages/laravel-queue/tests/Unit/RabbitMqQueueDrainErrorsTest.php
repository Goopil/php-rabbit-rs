<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Log;

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

it('drainSettlementErrors logs non-connection errors at warning level', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'AlreadySettled',
        'message' => 'delivery already settled',
    ]);

    Log::spy();
    $queue->drainSettlementErrors();

    Log::shouldHaveReceived('warning');
});

it('drainSettlementErrors logs MaxAttempts errors at error level', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'MaxAttempts',
        'message' => 'delivery attempts 25 exceed the configured maximum of 20 — acknowledged and dropped (no dead-letter exchange configured)',
        'message_id' => 'msg-poison-1',
        'attempts' => 25,
    ]);

    Log::spy();
    $queue->drainSettlementErrors();

    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled'
        && str_contains((string) ($context['message'] ?? ''), 'acknowledged and dropped')
        && ($context['attempts'] ?? null) === 25);
});

it('drainSettlementErrors logs a refused delayed release at error level without throwing', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('default');
    $consumer->pushError([
        'error_kind' => 'InvalidDelay',
        'message' => 'delay exceeds the largest configured TTL bucket (30000 ms); message msg-late rejected with requeue=false toward the dead-letter exchange',
        'message_id' => 'msg-late',
    ]);

    Log::spy();
    $queue->drainSettlementErrors();

    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled'
        && str_contains((string) ($context['message'] ?? ''), '30000 ms'));
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

it('drainSettlementErrors throws ConnectionException for a pipelined publish transport failure', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError([
        'kind' => 'Transport',
        'message_id' => 'msg-1',
        'message' => 'transport failed during confirmation',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(ConnectionException::class);
});

it('drainSettlementErrors throws QueueException for a returned publish', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError([
        'kind' => 'Returned',
        'message_id' => 'msg-2',
        'message' => 'message msg-2 was returned as unroutable (AMQP 312)',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(\Goopil\RabbitRs\Laravel\Exceptions\QueueException::class);
});

it('drainSettlementErrors clears pipelined publish errors after draining', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError([
        'kind' => 'Returned',
        'message_id' => 'msg-3',
        'message' => 'message msg-3 was returned as unroutable (AMQP 312)',
    ]);

    try {
        $queue->drainSettlementErrors();
    } catch (\Throwable) {
        // Expected: the returned publish surfaces as an exception.
    }

    expect($pool->drainErrors())->toBe([]);
});

it('pop surfaces a pipelined publish failure before fetching deliveries', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError([
        'kind' => 'Closed',
        'message_id' => 'msg-4',
        'message' => 'native client pool is closed',
    ]);

    expect(fn () => $queue->pop('orders-eu'))
        ->toThrow(\Goopil\RabbitRs\Laravel\Exceptions\QueueException::class);
});
