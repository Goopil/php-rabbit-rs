<?php

declare(strict_types=1);

describe('consume-side publish buffer flush', function () {
    it('flushes buffered publishes before the consumer waits for deliveries', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack', 'ack'],
            'deliveries' => [],
        ]);

        // Two publishes below the buffer threshold stay buffered: no threshold
        // is reached and the interval armed by the first publish has not
        // elapsed yet (the two publishes are back-to-back).
        $pool->publish(pubMessage('buffered-1', 'payload', [], 5000));
        $pool->publish(pubMessage('buffered-2', 'payload', [], 5000));
        expect($pool->stats()['publishes_total'])->toBe(0);

        // The consume path must drain the publish buffer first so a consumer
        // is never starved by publications still held in process memory.
        $consumer = $pool->consumer('main');
        $consumer->next(10);
        $consumer->close();

        expect($pool->stats()['publishes_total'])->toBe(2);

        $pool->close();
    });

    it('flushes buffered publishes on tryNext and nextBatch too', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
            'deliveries' => [],
        ]);

        $pool->publish(pubMessage('buffered-trynext', 'payload', [], 5000));
        expect($pool->stats()['publishes_total'])->toBe(0);

        $consumer = $pool->consumer('main');
        $consumer->tryNext();
        expect($pool->stats()['publishes_total'])->toBe(1);
        $consumer->close();

        $pool->close();
    });

    it('drops deadline-expired publications flushed from the consume path', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack', 'ack'],
            'deliveries' => [],
        ]);

        // The first publication expires while buffered: its failure is
        // definitive, mirroring the core actor's expire_replay. Re-buffering
        // it would poison every later flush with a permanently failing
        // publish; dropping it preserves the at-least-once contract because
        // the raised error is loud about the loss.
        $pool->publish(pubMessage('expired', 'payload', [], 1));
        $pool->publish(pubMessage('live', 'payload', [], 30000));
        usleep(50_000);

        // The flush fails loudly because of the expired publication; the
        // live one was still sent with that batch and is in-flight. Both
        // publications were accepted, so both are counted.
        try {
            $consumer = $pool->consumer('main');
            $consumer->next(10);
            expect(false)->toBeTrue('deadline-expired publication must fail loudly');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('deadline expired');
        }
        expect($pool->stats()['publishes_total'])->toBe(2);

        // The retry flush must not resurrect the expired publication: it
        // republishes only the live one and succeeds without raising.
        $consumer->next(10);
        expect($pool->stats()['publishes_total'])->toBe(3);

        $consumer->close();
        $pool->close();
    });
});
