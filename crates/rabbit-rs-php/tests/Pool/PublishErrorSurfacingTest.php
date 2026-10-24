<?php

declare(strict_types=1);

describe('publish error surfacing', function () {
    // The tests below pin publications as buffered until the explicit
    // flush(), so the interval timer must stay out of the way (one hour =
    // disabled for the scenario, like the re-buffer tests).
    $noTimer = ['buffer_flush_interval_ms' => 3_600_000];

    it('records the surplus returned outcomes of a sync flush for the next operation', function () use ($noTimer) {
        $pool = testingPool(defaultConfig(), $noTimer + [
            'publication_outcomes' => ['returned', 'returned'],
        ]);

        try {
            $pool->publish(pubMessage('first-returned'));
            $pool->publish(pubMessage('second-returned'));

            try {
                $pool->flush();
                expect(false)->toBeTrue('the first returned publication must throw');
            } catch (\Goopil\RabbitRs\Exception $e) {
                expect($e->getMessage())->toContain('first-returned');
            }

            // The flush raised the first unroutable outcome; the surplus one
            // must not be discarded silently: it stays queued and surfaces at
            // the next operation (drainErrors() reads the pending-error queue
            // without raising).
            $errors = $pool->drainErrors();
            expect($errors)->toHaveCount(1);
            expect($errors[0]['kind'])->toBe('Returned');
            expect($errors[0]['message_id'])->toBe('second-returned');
            expect($errors[0]['message'])->toContain('unroutable');

            expect($pool->stats()['publish_buffered'])->toBe(0);
            expect($pool->stats()['publishes_total'])->toBe(2);
        } finally {
            $pool->close();
        }
    });

    it('counts the discarded surplus error records when a pending backlog is surfaced', function () use ($noTimer) {
        $pool = testingPool(defaultConfig(), $noTimer + [
            'publication_outcomes' => ['returned', 'returned', 'returned'],
        ]);

        try {
            foreach (['first', 'second', 'third'] as $prefix) {
                $pool->publish(pubMessage("{$prefix}-returned"));
            }

            try {
                $pool->flush();
                expect(false)->toBeTrue('the first returned publication must throw');
            } catch (\Goopil\RabbitRs\Exception $e) {
                expect($e->getMessage())->toContain('first-returned');
            }

            // The next operation surfaces the second record; the surplus
            // third is discarded from the bounded pending-error queue.
            try {
                $pool->stats();
                expect(false)->toBeTrue('the second returned publication must surface');
            } catch (\Goopil\RabbitRs\Exception $e) {
                expect($e->getMessage())->toContain('second-returned');
            }

            // The discarded surplus record must be counted in the metrics
            // instead of vanishing silently.
            expect($pool->stats()['dropped_error_records_total'])->toBe(1);
        } finally {
            $pool->close();
        }
    });
});
