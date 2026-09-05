<?php

declare(strict_types=1);

describe('publish buffer backpressure', function () {
    it('raises backpressure when the publish buffer cannot drain', function () {
        // Blocked transport: no pre-pushed confirmations and a publisher
        // capacity of 1, so every attempted drain fails (core backpressure or
        // the mock's terminal NotRequested unconfirmed resolution) and
        // re-buffers its batch — the application-side publish buffer never
        // drains.
        //
        // The auto-flush triggers are pinned above the ceiling (interval of
        // one hour, threshold of 8192 messages) so no pipelined drain ever
        // engages: pipelined drains re-buffer their failed batch and the next
        // publish takes it again, so under loaded CI scheduling the buffer can
        // stay near-empty while publications cycle buffer -> drain -> buffer,
        // and the 30 s default deadline expires on starved runners, dropping
        // re-buffered publications. With both triggers disabled the ceiling is
        // reached through the synchronous overflow path instead, which is
        // scheduling-independent (issue #139). The one-hour deadline keeps the
        // re-buffer filter from dropping anything mid-test.
        //
        // Backpressure surfaces in two forms under the pipelined flush
        // (Round D): a surfaced drain record (the flush could not be
        // attempted) or the publish buffer refusing when it is full. Both
        // raise BackpressureException; this test pins the buffer ceiling
        // itself.
        $pool = testingPool(defaultConfig(), [
            'publisher_capacity' => 1,
            'buffer_flush_interval_ms' => 3_600_000,
            'buffer_flush_threshold' => 8192,
        ]);

        // PUBLISH_BUFFER_MAX_MESSAGES = 4096; beyond that, publish() refuses.
        // Re-buffered publications may exceed the ceiling (already accepted),
        // so the loop keeps going until the explicit refusal lands.
        $message = pubMessage('backpressure', str_repeat('x', 64), [], 3_600_000);

        $refused = null;
        for ($i = 0; $i < 16384; $i++) {
            try {
                $pool->publish($message);
            } catch (\Goopil\RabbitRs\BackpressureException $exception) {
                if (str_contains($exception->getMessage(), 'publish buffer is full')) {
                    $refused = $exception;
                    break;
                }
                // Surfaced drain backpressure: the batch could not be
                // attempted and was re-buffered; keep publishing.
            } catch (\Goopil\RabbitRs\Exception) {
                // A surfaced terminal outcome or drain failure: the
                // publication was re-buffered with its message_id; keep
                // publishing.
            }
        }
        expect($refused)->not->toBeNull('full publish buffer must raise backpressure');
        // Already-accepted messages are never dropped: the caller is told to
        // retry later.
        expect($refused->getMessage())->toContain('retry after flush');

        $pool->close();
    });

    it('surfaces a drain backpressure record at the next publish', function () {
        // Publisher capacity 1 with a blocked publisher: the first threshold
        // flush fails backpressure and re-buffers; the next publish must
        // surface that failure instead of silently continuing.
        $pool = testingPool(defaultConfig(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);
        $message = pubMessage('backpressure-record', str_repeat('x', 64), [], 30000);

        $surfaced = null;
        for ($i = 0; $i < 8192; $i++) {
            try {
                $pool->publish($message);
            } catch (\Goopil\RabbitRs\BackpressureException $exception) {
                $surfaced = $exception;
                break;
            } catch (\Goopil\RabbitRs\Exception) {
                // Other surfaced outcomes; keep publishing until the
                // backpressure record lands.
            }
        }

        expect($surfaced)->not->toBeNull('drain backpressure must surface at the next publish');
        expect($surfaced->getMessage())->toContain('capacity');

        $pool->close();
    });
});
