<?php

declare(strict_types=1);

describe('time-based publish buffer flush', function () {
    it('time-flushes the first publish once the interval elapses', function () {
        $pool = testingPool(defaultConfig(), ['publication_outcomes' => ['ack', 'ack']]);

        // The first publication of a fresh buffer stays below the threshold
        // but must schedule the interval deadline (issue #96).
        $pool->publish(pubMessage('first-flush-window', timeoutMs: 30000));
        expect($pool->stats()['publishes_total'])->toBe(0);

        usleep(5_000); // BUFFER_FLUSH_INTERVAL is 1 ms of wall time.

        // The next publish evaluates the armed interval and flushes the
        // whole batch, including the publication that waited. The pipelined
        // flush spawns on the runtime, so actor acceptance is observed
        // shortly after the trigger instead of synchronously.
        $pool->publish(pubMessage('second-triggers-flush', timeoutMs: 30000));
        $deadline = microtime(true) + 2;
        while (microtime(true) < $deadline && $pool->stats()['publishes_total'] < 2) {
            usleep(1000);
        }

        expect($pool->stats()['publishes_total'])->toBe(2);

        $pool->close();
    });
});
