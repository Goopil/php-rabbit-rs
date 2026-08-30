<?php

declare(strict_types=1);

describe('native event drain', function () {
    it('invokes connection state callbacks during publish without stats()', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
        ]);

        $states = [];
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states): void {
            $states[] = [$broker, $state, $generation];
        });

        $pool->publish(pubMessage('event-publish'));
        $pool->flush();

        expect($states)->not->toBeEmpty('callback must fire on publish, not only on stats()');

        $pool->close();
    });

    it('invokes connection state callbacks during consumer next() without stats()', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [],
        ]);

        $states = [];
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states): void {
            $states[] = [$broker, $state, $generation];
        });

        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(5);
        expect($delivery)->toBeNull();
        expect($states)->not->toBeEmpty('callback must fire on consume, not only on stats()');

        $pool->close();
    });

    it('invokes backpressure callbacks during publishBatch without stats()', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);

        $backpressure = [];
        $pool->onBackpressure(function (string $broker, int $inFlight, int $capacity) use (&$backpressure): void {
            $backpressure[] = [$broker, $inFlight, $capacity];
        });

        try {
            $pool->publishBatch([
                pubMessage('bp-event-1', 'payload', [], 1),
                pubMessage('bp-event-2', 'payload', [], 1),
            ]);
        } catch (\Goopil\RabbitRs\BackpressureException) {
            // Expected: exceeding capacity raises backpressure.
        }

        expect($backpressure)->not->toBeEmpty('callback must fire on publishBatch, not only on stats()');

        $pool->close();
    });
});
