<?php

declare(strict_types=1);

use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
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

it('surfaces every drained publish failure across successive calls', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError(['kind' => 'Transport', 'message_id' => 'one', 'message' => 'transport one']);
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'two', 'message' => 'returned two']);

    expect(fn () => $queue->drainSettlementErrors())->toThrow(ConnectionException::class, 'transport one');
    expect(fn () => $queue->drainSettlementErrors())->toThrow(QueueException::class, 'returned two');
    $queue->drainSettlementErrors();
});

it('surfaces a retained publish failure before the next queue operation', function (string $operation): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'one', 'message' => 'returned one']);
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'two', 'message' => 'returned two']);
    expect(fn () => $queue->drainSettlementErrors())->toThrow(QueueException::class, 'returned one');

    expect(fn () => match ($operation) {
        'push' => $queue->pushRaw('{}'),
        'size' => $queue->size(),
        'clear' => $queue->clear(),
        'stats' => $queue->stats(),
        'bulk' => $queue->bulk(['ExampleJob']),
    })->toThrow(QueueException::class, 'returned two');
})->with(['push', 'size', 'clear', 'stats', 'bulk']);

it('preserves backpressure type and message identity on a drained publish error', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError(['kind' => 'Backpressure', 'message_id' => 'message-123', 'message' => 'pipeline saturated']);

    expect(fn () => $queue->drainSettlementErrors())->toThrow(BackpressureException::class, 'message-123');
});

it('identifies a publish even when its id appears incidentally in the error text', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'x', 'message' => 'maximum capacity']);

    expect(fn () => $queue->drainSettlementErrors())->toThrow(QueueException::class, 'message x: maximum capacity');
});

it('continues draining later settlement errors if the logger throws', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError(['delivery_tag' => 1, 'subscription' => 'auto', 'error_kind' => 'AlreadySettled', 'message' => 'first settlement']);
    $consumer->pushError(['delivery_tag' => 2, 'subscription' => 'auto', 'error_kind' => 'MaxAttempts', 'message' => 'second settlement']);
    Log::shouldReceive('warning')->once()->andThrow(new RuntimeException('logger unavailable'));
    Log::shouldReceive('error')->once()->withArgs(fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled' && ($context['delivery_tag'] ?? null) === 2);

    $queue->drainSettlementErrors();
});

it('retains later settlement errors when a connection error interrupts the drain', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError(['delivery_tag' => 1, 'subscription' => 'auto', 'error_kind' => 'Transport', 'message' => 'link down']);
    $consumer->pushError(['delivery_tag' => 2, 'subscription' => 'auto', 'error_kind' => 'MaxAttempts', 'message' => 'poison two']);
    Log::spy();

    expect(fn () => $queue->drainSettlementErrors())->toThrow(ConnectionException::class, 'link down');
    $queue->drainSettlementErrors();
    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled' && ($context['delivery_tag'] ?? null) === 2)->once();
});

it('retains settlement errors when the logger reenters the drain', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError(['delivery_tag' => 1, 'subscription' => 'auto', 'error_kind' => 'AlreadySettled', 'message' => 'first']);
    $consumer->pushError(['delivery_tag' => 2, 'subscription' => 'auto', 'error_kind' => 'Transport', 'message' => 'link down']);
    $consumer->pushError(['delivery_tag' => 3, 'subscription' => 'auto', 'error_kind' => 'MaxAttempts', 'message' => 'poison']);
    $reentered = false;
    Log::shouldReceive('warning')->once()->andReturnUsing(function () use ($queue, &$reentered): void {
        if (! $reentered) {
            $reentered = true;
            $queue->drainSettlementErrors();
        }
    });
    Log::shouldReceive('error')->once()->withArgs(fn (string $message, array $context): bool => ($context['delivery_tag'] ?? null) === 3);

    expect(fn () => $queue->drainSettlementErrors())->toThrow(ConnectionException::class, 'link down');
    $queue->drainSettlementErrors();
    expect($reentered)->toBeTrue();
});

it('drains existing settlement errors before closing an older consumer', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->rejectDrainAfterClose = true;
    $consumer->pushError(['delivery_tag' => 41, 'subscription' => 'auto', 'error_kind' => 'MaxAttempts', 'message' => 'before close']);
    Log::spy();

    $queue->closeConsumers();
    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => ($context['delivery_tag'] ?? null) === 41)->once();
});

it('logs a retained publish failure if the queue is destroyed before the next drain', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'one', 'message' => 'returned one']);
    $pool->pushPublishError(['kind' => 'Returned', 'message_id' => 'two', 'message' => 'returned two']);
    Log::spy();

    try {
        $queue->drainSettlementErrors();
        $this->fail('The first returned publication must be surfaced');
    } catch (QueueException $exception) {
        expect($exception->getMessage())->toContain('returned one');
    }
    unset($exception);
    $queue->__destruct();
    unset($queue);
    gc_collect_cycles();
    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => str_contains($message, 'never surfaced') && ($context['message_id'] ?? null) === 'two')->once();
});

it('logs retained settlement failures when consumers close before the next drain', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError(['delivery_tag' => 1, 'subscription' => 'auto', 'error_kind' => 'Transport', 'message' => 'link down']);
    $consumer->pushError(['delivery_tag' => 2, 'subscription' => 'auto', 'error_kind' => 'InvalidDelay', 'message' => 'poison two']);
    Log::spy();

    expect(fn () => $queue->drainSettlementErrors())->toThrow(ConnectionException::class, 'link down');
    $queue->closeConsumers();
    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled' && ($context['delivery_tag'] ?? null) === 2)->once();
});

it('continues reporting settlement errors when a logger call fails during close', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError(['delivery_tag' => 1, 'subscription' => 'auto', 'error_kind' => 'AlreadySettled', 'message' => 'first settlement']);
    $consumer->pushError(['delivery_tag' => 2, 'subscription' => 'auto', 'error_kind' => 'MaxAttempts', 'message' => 'second settlement']);
    Log::shouldReceive('warning')->once()->andThrow(new RuntimeException('logger unavailable'));
    Log::shouldReceive('error')->once()->withArgs(fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled' && ($context['delivery_tag'] ?? null) === 2);

    $queue->closeConsumers();
});

it('logs a settlement failure produced while the consumer closes', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->onClose(fn () => $consumer->pushError([
        'delivery_tag' => 42,
        'subscription' => 'auto',
        'error_kind' => 'MaxAttempts',
        'message' => 'settlement failed during close',
    ]));
    Log::spy();

    $queue->closeConsumers();
    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled' && ($context['delivery_tag'] ?? null) === 42)->once();
});

it('drainSettlementErrors throws ConnectionException on StaleGeneration error', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
        'error_kind' => 'StaleGeneration',
        'message' => 'stale generation detected',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(ConnectionException::class);
});

it('drainSettlementErrors throws ConnectionException on Transport error', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
        'error_kind' => 'Transport',
        'message' => 'transport error',
    ]);

    expect(fn () => $queue->drainSettlementErrors())
        ->toThrow(ConnectionException::class);
});

it('drainSettlementErrors does not throw on non-connection errors', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
        'error_kind' => 'AlreadySettled',
        'message' => 'delivery already settled',
    ]);

    $queue->drainSettlementErrors();

    expect(true)->toBeTrue();
});

it('drainSettlementErrors clears errors after draining', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
        'error_kind' => 'AlreadySettled',
        'message' => 'delivery already settled',
    ]);

    $queue->drainSettlementErrors();

    expect($consumer->drainErrors())->toBe([]);
});

it('drainSettlementErrors logs non-connection errors at warning level', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
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
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
        'error_kind' => 'MaxAttempts',
        'message' => 'delivery attempts 25 exceed the configured maximum of 20 — acknowledged and dropped (no dead-letter exchange configured)',
    ]);

    Log::spy();
    $queue->drainSettlementErrors();

    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled'
        && str_contains((string) ($context['message'] ?? ''), 'acknowledged and dropped')
        && ($context['delivery_tag'] ?? null) === 1
        && ($context['subscription'] ?? null) === 'auto');
});

it('drainSettlementErrors logs a refused delayed release at error level without throwing', function (): void {
    [$queue, $pool] = makeDrainQueue();
    warmConsumerCache($queue);
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 2,
        'subscription' => 'auto',
        'error_kind' => 'InvalidDelay',
        'message' => 'delay exceeds the largest configured TTL bucket (30000 ms); message msg-late rejected with requeue=false toward the dead-letter exchange',
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
    $consumer = $pool->consumerFor('__auto__.orders-eu');
    $consumer->pushError([
        'delivery_tag' => 1,
        'subscription' => 'auto',
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
        ->toThrow(QueueException::class);
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
    } catch (Throwable) {
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
        ->toThrow(QueueException::class);
});

it('logs a returned publish at teardown when no operation surfaced it', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->pushPublishError([
        'kind' => 'Returned',
        'message_id' => 'msg-teardown-1',
        'message' => 'message msg-teardown-1 was returned as unroutable (AMQP 312)',
    ]);

    Log::spy();
    $caught = null;
    try {
        unset($queue);
        gc_collect_cycles();
    } catch (Throwable $exception) {
        $caught = $exception;
    }

    expect($caught)->toBeNull('teardown must never throw');

    Log::shouldHaveReceived('error', fn (string $message, array $context): bool => str_contains($message, 'never surfaced')
        && ($context['kind'] ?? null) === 'Returned'
        && ($context['message_id'] ?? null) === 'msg-teardown-1'
        && str_contains((string) ($context['message'] ?? ''), 'unroutable'));
});

it('logs nothing at teardown when every publish outcome was surfaced', function (): void {
    [$queue] = makeDrainQueue();

    Log::spy();
    $caught = null;
    try {
        unset($queue);
        gc_collect_cycles();
    } catch (Throwable $exception) {
        $caught = $exception;
    }

    expect($caught)->toBeNull();

    Log::shouldNotHaveReceived('error');
});

it('never throws from teardown when the pool drain fails', function (): void {
    [$queue, $pool] = makeDrainQueue();
    $pool->throwOnNextDrainErrors(new RuntimeException('pool closed mid-teardown'));

    Log::spy();
    $caught = null;
    try {
        unset($queue);
        gc_collect_cycles();
    } catch (Throwable $exception) {
        $caught = $exception;
    }

    expect($caught)->toBeNull();

    Log::shouldNotHaveReceived('error');
});

it('teardown of a container-less queue logs nothing and never throws', function (): void {
    $pool = new Pool(['workers' => popWorkers()]);
    $queue = new RabbitMqQueue(
        $pool,
        popRoutes(),
        'default',
        workerProfiles: new WorkerProfileResolver(popWorkers()),
    );
    $pool->pushPublishError([
        'kind' => 'Returned',
        'message_id' => 'msg-teardown-2',
        'message' => 'message msg-teardown-2 was returned as unroutable (AMQP 312)',
    ]);

    $caught = null;
    try {
        unset($queue);
        gc_collect_cycles();
    } catch (Throwable $exception) {
        $caught = $exception;
    }

    expect($caught)->toBeNull();
});
