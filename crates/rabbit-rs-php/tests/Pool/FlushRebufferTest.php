<?php

declare(strict_types=1);

describe('flush re-buffering', function () {
    it('re-buffers and retries a publication whose batch flush failed', function () {
        $pool = testingPool(defaultConfig(), [
            'publication_outcomes' => ['transport_error', 'ack'],
        ]);

        try {
            $pool->publish(pubMessage('flush-retry'));
            $pool->flush();
            expect(false)->toBeTrue('failed flush must throw');
        } catch (\Goopil\RabbitRs\ConnectionException $e) {
            expect($e->getMessage())->toContain('transport failed');
        }

        // The unconfirmed message must be re-buffered: the next flush
        // republishes it. The duplicate is permitted and identifiable
        // through its message_id (at-least-once contract).
        $pool->flush();
        expect($pool->stats()['publishes_total'])->toBe(2);

        $pool->close();
    });

    it('does not re-buffer a message returned as unroutable', function () {
        $pool = testingPool(defaultConfig(), [
            'publication_outcomes' => ['returned', 'ack'],
        ]);

        try {
            $pool->publish(pubMessage('unroutable'));
            $pool->flush();
            expect(false)->toBeTrue('returned publication must throw');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('unroutable');
            expect($e->getMessage())->toContain('312');
        }

        // Unroutable is definitive: the next flush must be a no-op instead of
        // republishing the returned message in an endless loop.
        $pool->flush();
        expect($pool->stats()['publishes_total'])->toBe(1);

        $pool->close();
    });

    it('retries re-buffered messages before newer buffered messages', function () {
        $pool = testingPool(defaultConfig(), [
            'publication_outcomes' => ['transport_error', 'returned', 'ack'],
        ]);

        try {
            $pool->publish(pubMessage('older'));
            $pool->flush();
            expect(false)->toBeTrue('failed flush must throw');
        } catch (\Goopil\RabbitRs\ConnectionException) {
            // Best-effort: ignore errors during cleanup/teardown.
        }

        try {
            $pool->publish(pubMessage('newer'));
            $pool->flush();
            expect(false)->toBeTrue('retried older message must fail again');
        } catch (\Goopil\RabbitRs\Exception $e) {
            // The re-buffered message is retried first, so it consumes the
            // 'returned' confirmation before the newer message is published.
            expect($e->getMessage())->toContain('older');
        }

        expect($pool->stats()['publishes_total'])->toBe(3);

        // The returned message is not re-buffered again: no infinite loop.
        $pool->flush();
        expect($pool->stats()['publishes_total'])->toBe(3);

        $pool->close();
    });
});
