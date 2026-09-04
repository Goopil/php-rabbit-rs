<?php

declare(strict_types=1);

describe('pipelined auto-flush', function () {
    it('returns from publish before confirmations resolve and quiesces on flush', function () {
        $pool = testingPool(defaultConfig(), ['pending_confirmations' => 80]);

        $start = hrtime(true);
        for ($i = 0; $i < 64; $i++) {
            $pool->publish(pubMessage("m{$i}", timeoutMs: 1000));
        }
        $loopMs = (hrtime(true) - $start) / 1e6;

        // The threshold auto-flush spawns on the runtime: the PHP thread must
        // not park for the confirm window (a blocking flush would hold the
        // batch until the 1 s per-message deadline expires and throw).
        expect($loopMs)->toBeLessThan(500.0);

        // An explicit flush quiesces the spawned drain and stays bounded.
        $start = hrtime(true);
        $pool->flush();
        $flushMs = (hrtime(true) - $start) / 1e6;
        expect($flushMs)->toBeLessThan(2000.0);

        expect($pool->stats()['publishes_total'])->toBe(64);
        expect($pool->stats()['dropped_publications_total'])->toBe(0);

        $pool->close();
    });

    it('surfaces a returned publication at the next drainErrors without re-publishing it', function () {
        $outcomes = array_merge(['returned'], array_fill(0, 63, 'ack'));
        $pool = testingPool(defaultConfig(), ['publication_outcomes' => $outcomes]);

        for ($i = 0; $i < 64; $i++) {
            $pool->publish(pubMessage("m{$i}"));
        }

        $record = waitForPublishError($pool);
        expect($record)->not->toBeNull('returned outcome must surface via drainErrors');
        expect($record['kind'])->toBe('Returned');
        expect($record['message'])->toContain('unroutable');
        expect($record['message'])->toContain('312');
        expect($record['message_id'])->toBe('m0');

        // Drained: a second call returns nothing.
        expect($pool->drainErrors())->toBe([]);

        // Unroutable is definitive: the next flush must not re-publish it.
        $pool->flush();
        expect($pool->stats()['publishes_total'])->toBe(64);

        $pool->close();
    });

    it('surfaces a terminal transport failure at stats and retries the re-buffered batch', function () {
        $outcomes = array_merge(['transport_error'], array_fill(0, 127, 'ack'));
        $pool = testingPool(defaultConfig(), ['publication_outcomes' => $outcomes]);

        for ($i = 0; $i < 64; $i++) {
            $pool->publish(pubMessage("m{$i}"));
        }

        $thrown = null;
        $deadline = microtime(true) + 5;
        while (microtime(true) < $deadline) {
            try {
                $pool->stats();
            } catch (\Goopil\RabbitRs\ConnectionException $exception) {
                $thrown = $exception;
                break;
            }
            usleep(2000);
        }

        expect($thrown)->not->toBeNull('a pending publish failure must surface at stats');
        expect($thrown->getMessage())->toContain('transport failed');

        // Surfaced exactly once: the next stats is clean.
        expect($pool->stats()['publishes_total'])->toBe(64);

        // The failed batch is re-buffered and retried on the next flush
        // (at-least-once): duplicates permitted, identifiable via message_id.
        $pool->flush();
        expect($pool->stats()['publishes_total'])->toBe(128);
        expect($pool->stats()['dropped_publications_total'])->toBe(0);

        $pool->close();
    });

    it('reports no pending records when every publication confirms', function () {
        $outcomes = array_fill(0, 64, 'ack');
        $pool = testingPool(defaultConfig(), ['publication_outcomes' => $outcomes]);

        for ($i = 0; $i < 64; $i++) {
            $pool->publish(pubMessage("m{$i}"));
        }

        $deadline = microtime(true) + 2;
        while (microtime(true) < $deadline && $pool->stats()['confirmations_total'] < 64) {
            usleep(2000);
        }

        expect($pool->drainErrors())->toBe([]);
        expect($pool->stats()['publishes_total'])->toBe(64);
        expect($pool->stats()['dropped_publications_total'])->toBe(0);

        $pool->close();
    });
});

/**
 * Polls drainErrors until a record lands (the pipelined drain resolves
 * asynchronously on the runtime). Gives up after 5 seconds.
 *
 * @return array{kind: string, message_id: string, message: string}|null
 */
function waitForPublishError(\Goopil\RabbitRs\Pool $pool): ?array
{
    $deadline = microtime(true) + 5;
    while (microtime(true) < $deadline) {
        $errors = $pool->drainErrors();
        if ($errors !== []) {
            return $errors[0];
        }
        usleep(2000);
    }

    return null;
}
