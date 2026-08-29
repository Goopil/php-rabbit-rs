<?php

declare(strict_types=1);

function backpressureMessage(string $messageId): array
{
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => 'payload',
        'message_id' => $messageId,
        'headers' => [
            'trace.sampled' => true,
            'trace.source' => 'native',
        ],
        'timeout_ms' => 1,
    ];
}

describe('backpressure', function () {
    it('throws BackpressureException when publisher capacity is exceeded', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);
        $consumer = $pool->consumer('main');

        try {
            $pool->publishBatch([backpressureMessage('first'), backpressureMessage('second')]);
            expect(false)->toBeTrue('bounded publisher must apply backpressure');
        } catch (\Goopil\RabbitRs\BackpressureException $e) {
            expect($e->getMessage())->toContain('capacity');
        }

        $pool->close();
    });

    it('increments the backpressure metric in stats', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);

        try {
            $pool->publishBatch([backpressureMessage('first'), backpressureMessage('second')]);
        } catch (\Goopil\RabbitRs\BackpressureException) {
            // Expected: publish exceeds capacity and must surface backpressure.
        }

        expect($pool->stats()['backpressure_total'])->toBe(1);

        $pool->close();
    });

    it('terminates the active consumer when pool is closed', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);
        $consumer = $pool->consumer('main');

        $pool->close();

        try {
            $consumer->next(1);
            expect(false)->toBeTrue('pool close must terminate its active consumer');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('closed');
        }
    });
});
