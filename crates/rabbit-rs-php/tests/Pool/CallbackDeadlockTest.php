<?php

declare(strict_types=1);

describe('callback deadlock', function () {
    it('connection state callback calling stats does not deadlock', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
        ]);

        $reentered = false;
        $pool->onConnectionState(function () use ($pool, &$reentered) {
            // Re-enter stats() from inside the connection-state callback.
            // Before the fix this deadlocks because invoke_connection_state_callbacks
            // holds last_connection_states while calling the PHP callback,
            // and stats() tries to acquire the same mutex.
            $pool->stats();
            $reentered = true;
        });

        // Publishing starts the coordinator, which reaches Ready.
        // The next stats() call sees the state change and fires the callback.
        $pool->publish(pubMessage('deadlock-test'));
        $pool->flush();

        // This call must complete without hanging.
        $stats = $pool->stats();
        expect($stats)->toBeArray();
        expect($reentered)->toBeTrue();

        $pool->close();
    });

    it('backpressure callback calling stats does not deadlock', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);

        $reentered = false;
        $pool->onBackpressure(function () use ($pool, &$reentered) {
            // Re-enter stats() from inside the backpressure callback.
            // Before the fix this deadlocks because invoke_backpressure_callback
            // holds last_backpressure_total while calling the PHP callback,
            // and stats() tries to acquire the same mutex.
            $pool->stats();
            $reentered = true;
        });

        // Trigger backpressure by exceeding capacity.
        try {
            $pool->publishBatch([
                pubMessage('bp-1', 'payload', [], 1),
                pubMessage('bp-2', 'payload', [], 1),
            ]);
        } catch (\Goopil\RabbitRs\BackpressureException) {
        }

        // This call must complete without hanging.
        $stats = $pool->stats();
        expect($stats)->toBeArray();
        expect($reentered)->toBeTrue();

        $pool->close();
    });
});
