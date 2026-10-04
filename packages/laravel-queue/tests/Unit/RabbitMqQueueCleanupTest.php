<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;
use Illuminate\Support\Facades\Log;

/**
 * @return list<array<string, mixed>>
 */
function cleanupWorkers(): array
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
function cleanupRoutes(): array
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
function makeCleanupQueue(string $defaultQueue = 'default'): array
{
    $pool = new Pool(['workers' => cleanupWorkers()]);
    $resolver = new WorkerProfileResolver(cleanupWorkers());
    $queue = new RabbitMqQueue(
        $pool,
        cleanupRoutes(),
        $defaultQueue,
        workerProfiles: $resolver,
    );
    $queue->setContainer(new Container);

    return [$queue, $pool];
}

describe('closeConsumers', function (): void {
    it('closes all cached consumers', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        // Create two consumers by popping from two different profiles.
        $queue->pop('orders-eu');
        $queue->pop('urgent-eu');

        $consumer1 = $pool->consumerFor('__auto__.orders-eu');
        $consumer2 = $pool->consumerFor('high-priority');

        expect(0)->toBe($consumer1->closeCalls)
            ->and(0)->toBe($consumer2->closeCalls);

        $queue->closeConsumers();

        expect(1)->toBe($consumer1->closeCalls)
            ->and(1)->toBe($consumer2->closeCalls);
    });

    it('clears the consumer cache', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        expect(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);

        $queue->closeConsumers();

        // After closeConsumers, calling pop again must create a new consumer.
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        expect(['__auto__.orders-eu'])->toBe($pool->consumerProfiles);
    });

    it('is idempotent', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('__auto__.orders-eu');

        $queue->closeConsumers();
        $queue->closeConsumers();

        expect(1)->toBe($consumer->closeCalls);
    });

    it('is safe with no cached consumers', function (): void {
        [$queue] = makeCleanupQueue();

        // Should not throw.
        $queue->closeConsumers();

        expect(true)->toBeTrue();
    });

    it('creates a new consumer on pop after closeConsumers', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $firstConsumer = $pool->consumerFor('__auto__.orders-eu');

        $queue->closeConsumers();
        $pool->consumerProfiles = [];
        $queue->pop('orders-eu');
        $secondConsumer = $pool->consumerFor('__auto__.orders-eu');

        expect($firstConsumer)->not->toBe($secondConsumer);
    });

    it('logs undrained settlement errors before closing the consumer', function (): void {
        [$queue, $pool] = makeCleanupQueue();
        $queue->setContainer(app());

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('__auto__.orders-eu');
        $consumer->pushError([
            'error_kind' => 'AlreadySettled',
            'message' => 'delivery already settled',
            'message_id' => 'msg-settled-1',
        ]);

        Log::spy();
        $queue->closeConsumers();

        expect(1)->toBe($consumer->closeCalls, 'the consumer must still be closed');

        Log::shouldHaveReceived('warning', fn (string $message, array $context): bool => $message === 'rabbit-rs settlement error'
            && ($context['error_kind'] ?? null) === 'AlreadySettled'
            && ($context['message_id'] ?? null) === 'msg-settled-1');
    });

    it('logs poison settlement records at error level before closing the consumer', function (): void {
        [$queue, $pool] = makeCleanupQueue();
        $queue->setContainer(app());

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('__auto__.orders-eu');
        $consumer->pushError([
            'error_kind' => 'MaxAttempts',
            'message' => 'delivery attempts 25 exceed the configured maximum of 20 — acknowledged and dropped (no dead-letter exchange configured)',
            'message_id' => 'msg-poison-close',
            'attempts' => 25,
        ]);

        Log::spy();
        $queue->closeConsumers();

        Log::shouldHaveReceived('error', fn (string $message, array $context): bool => $message === 'rabbit-rs: poison delivery settled'
            && ($context['message_id'] ?? null) === 'msg-poison-close'
            && ($context['attempts'] ?? null) === 25);
    });

    it('never throws from closeConsumers when a pending settlement record is connection-level', function (): void {
        [$queue, $pool] = makeCleanupQueue();
        $queue->setContainer(app());

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('__auto__.orders-eu');
        $consumer->pushError([
            'error_kind' => 'StaleGeneration',
            'message' => 'stale generation detected',
        ]);

        Log::spy();
        $caught = null;
        try {
            $queue->closeConsumers();
        } catch (Throwable $exception) {
            $caught = $exception;
        }

        expect($caught)->toBeNull('consumer teardown must never throw')
            ->and(1)->toBe($consumer->closeCalls);

        Log::shouldHaveReceived('warning', fn (string $message, array $context): bool => $message === 'rabbit-rs settlement error'
            && ($context['error_kind'] ?? null) === 'StaleGeneration');
    });

    it('logs nothing when no settlement records are pending', function (): void {
        [$queue, $pool] = makeCleanupQueue();
        $queue->setContainer(app());

        $queue->pop('orders-eu');

        Log::spy();
        $queue->closeConsumers();

        Log::shouldNotHaveReceived('warning');
        Log::shouldNotHaveReceived('error');
    });
});

describe('destruct', function (): void {
    it('calls closeConsumers on destruct', function (): void {
        [$queue, $pool] = makeCleanupQueue();

        $queue->pop('orders-eu');
        $consumer = $pool->consumerFor('__auto__.orders-eu');

        expect(0)->toBe($consumer->closeCalls);

        unset($queue);

        expect(1)->toBe($consumer->closeCalls);
    });
});
