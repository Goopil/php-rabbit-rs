<?php

declare(strict_types=1);

// A pool whose size() skips the flush barrier stalls the admin-channel
// readiness wait (30 s default); the 1 s floor of consumer.wait_timeout
// keeps that failure mode loud and quick.
function barrierPool(array $scenario, string $messageId): \Goopil\RabbitRs\Pool
{
    $config = defaultConfig();
    $config['consumer'] = ['wait_timeout' => 1000];
    $pool = testingPool($config, $scenario);
    $pool->publish(pubMessage($messageId));

    // One publish below every flush trigger stays buffered: the stale-size
    // precondition of issue #209.
    expect($pool->stats()['publish_buffered'])->toBe(1);
    expect($pool->stats()['publishes_total'])->toBe(0);

    return $pool;
}

describe('size() flush barrier', function () {
    it('flushes a same-process publication before querying the broker in safe mode', function () {
        $pool = barrierPool(['publication_outcomes' => ['ack']], 'size-barrier-safe');

        try {
            // Safe-mode size() is a full barrier: the buffered publication is
            // confirmed by the time the count query returns.
            expect($pool->size('default', 'jobs'))->toBe(0);

            expect($pool->stats()['publish_buffered'])->toBe(0);
            expect($pool->stats()['publishes_total'])->toBe(1);
        } finally {
            $pool->close();
        }
    });

    it('hands off a same-process publication before querying the broker in blind mode', function () {
        $pool = barrierPool(['publisher_safety' => 'blind'], 'size-barrier-blind');

        try {
            // Blind size() is a hand-off barrier: the publication is enqueued
            // on the publish pump before the count query, and no pending error
            // surfaces through size().
            expect($pool->size('default', 'jobs'))->toBe(0);

            expect($pool->stats()['publish_buffered'])->toBe(0);
            expect($pool->stats()['publishes_total'])->toBe(1);
            expect($pool->drainErrors())->toBeEmpty();
        } finally {
            $pool->close();
        }
    });
});
