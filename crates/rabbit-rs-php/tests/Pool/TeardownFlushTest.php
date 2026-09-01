<?php

declare(strict_types=1);

describe('teardown flush budget and drop accounting', function () {
    it('counts deadline-expired publications as dropped', function () {
        $pool = testingPool(defaultConfig(), ['pending_confirmations' => 5]);
        $pool->publish(pubMessage('expired-1', timeoutMs: 1));
        $pool->publish(pubMessage('expired-2', timeoutMs: 1));

        try {
            $pool->flush();
            expect(false)->toBeTrue('a timed-out flush must throw');
        } catch (\Goopil\RabbitRs\Exception) {
            // Expected: the batch deadline expired while confirmations were
            // pending. The expired publications are definitive drops.
        }

        expect($pool->stats()['dropped_publications_total'])->toBe(2);

        $pool->close();
    });

    it('keeps full-deadline semantics on an explicit flush', function () {
        $pool = testingPool(defaultConfig(), ['pending_confirmations' => 5]);
        $pool->publish(pubMessage('explicit-flush', timeoutMs: 2000));

        $start = hrtime(true);
        try {
            $pool->flush();
            expect(false)->toBeTrue('a timed-out flush must throw');
        } catch (\Goopil\RabbitRs\Exception) {
            // Expected.
        }
        $elapsedMs = (hrtime(true) - $start) / 1e6;

        expect($elapsedMs)->toBeGreaterThanOrEqual(1500.0);
        expect($pool->stats()['dropped_publications_total'])->toBe(1);

        $pool->close();
    });

    it('bounds the destruct flush with a fixed shutdown budget', function () {
        $pool = testingPool(defaultConfig(), ['pending_confirmations' => 5]);
        $pool->publish(pubMessage('teardown-slow', timeoutMs: 30000));

        $start = hrtime(true);
        unset($pool);
        $elapsedMs = (hrtime(true) - $start) / 1e6;

        // A 30 s per-message deadline must not stall FPM shutdown: the
        // destruct flush runs on a fixed 500 ms budget.
        expect($elapsedMs)->toBeLessThan(2000.0);
    });
});
