<?php

declare(strict_types=1);

describe('publish buffer backpressure', function () {
    it('raises backpressure when the publish buffer cannot drain', function () {
        // Blocked transport: no pre-pushed confirmations and a publisher
        // capacity of 1, so every flush fails (core backpressure or the
        // mock's NotRequested unconfirmed resolution) and re-buffers its
        // batch — the application-side publish buffer never drains.
        // The deadline must outlive the loop: deadline-expired publications
        // are definitive and are not re-buffered, mirroring the core actor's
        // expire_replay behavior.
        //
        // Backpressure surfaces in two forms under the pipelined flush
        // (Round D): a surfaced drain record (the flush could not be
        // attempted) or the publish buffer refusing when it is full. Both
        // raise BackpressureException; this test pins the buffer ceiling
        // itself.
        $pool = testingPool(defaultConfig(), ['publisher_capacity' => 1]);

        // PUBLISH_BUFFER_MAX_MESSAGES = 4096; beyond that, publish() refuses.
        // Re-buffered publications may exceed the ceiling (already accepted),
        // so the loop keeps going until the explicit refusal lands.
        $message = pubMessage('backpressure', str_repeat('x', 64), [], 30000);

        $refused = null;
        $sightings = [];
        $okCount = 0;
        for ($i = 0; $i < 16384; $i++) {
            try {
                $pool->publish($message);
                $okCount++;
            } catch (\Goopil\RabbitRs\BackpressureException $exception) {
                if (str_contains($exception->getMessage(), 'publish buffer is full')) {
                    $refused = $exception;
                    break;
                }
                // Surfaced drain backpressure: the batch could not be
                // attempted and was re-buffered.
                $key = str_contains($exception->getMessage(), 'saturated')
                    ? 'saturated'
                    : 'other-backpressure: '.substr($exception->getMessage(), 0, 80);
                $sightings[$key] = ($sightings[$key] ?? 0) + 1;
            } catch (\Goopil\RabbitRs\Exception $exception) {
                // A surfaced terminal outcome or drain failure: the
                // publication was re-buffered with its message_id.
                $key = $exception::class.': '.substr($exception->getMessage(), 0, 80);
                $sightings[$key] = ($sightings[$key] ?? 0) + 1;
            }
        }

        if ($refused === null) {
            fwrite(STDERR, "DIAG ok={$okCount} sightings=".json_encode($sightings)."\n");
            try {
                fwrite(STDERR, 'DIAG stats='.json_encode($pool->stats()->getArrayCopy())."\n");
            } catch (\Throwable $throwable) {
                fwrite(STDERR, 'DIAG stats threw: '.$throwable->getMessage()."\n");
            }
            try {
                fwrite(STDERR, 'DIAG drainErrors='.json_encode($pool->drainErrors())."\n");
            } catch (\Throwable $throwable) {
                fwrite(STDERR, 'DIAG drainErrors threw: '.$throwable->getMessage()."\n");
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
