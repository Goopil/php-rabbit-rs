<?php

declare(strict_types=1);

describe('publish buffer backpressure', function () {
    it('raises backpressure when the publish buffer is full and cannot flush', function () {
        // Blocked transport: no pre-pushed confirmations and a publisher
        // capacity of 1, so every threshold flush fails (core pump
        // backpressure or unconfirmed publication) and re-buffers its
        // messages — the application-side publish buffer never drains.
        // The deadline must outlive the loop (~4000 failed flushes):
        // deadline-expired publications are definitive and are not
        // re-buffered, mirroring the core actor's expire_replay behavior.
        $pool = testingPool(defaultConfig(), ['publisher_capacity' => 1]);

        // PUBLISH_BUFFER_MAX_MESSAGES = 4096; beyond that, publish() refuses.
        $message = pubMessage('backpressure', str_repeat('x', 64), [], 30000);

        for ($i = 0; $i < 4096; $i++) {
            try {
                $pool->publish($message);
            } catch (\Goopil\RabbitRs\Exception) {
                // The threshold flush failed while the transport is down and
                // the message was re-buffered with its message_id.
            }
        }

        try {
            $pool->publish($message);
            expect(false)->toBeTrue('full publish buffer must raise backpressure');
        } catch (\Goopil\RabbitRs\BackpressureException $e) {
            // Already-accepted messages are never dropped: the buffer still
            // holds all 4096 of them and the caller is told to retry later.
            expect($e->getMessage())->toContain('4096 messages');
            expect($e->getMessage())->toContain('retry after flush');
        }

        $pool->close();
    });
});
