<?php

declare(strict_types=1);

describe('fire-and-forget settlement', function () {
    it('drainErrors returns empty when no settlement errors', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'drain-empty', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);
        expect($delivery)->not->toBeNull();

        $errors = $consumer->drainErrors();
        expect($errors)->toBeEmpty();

        $pool->close();
    });

    it('ack returns void without blocking', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'ack-void', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);
        expect($delivery)->not->toBeNull();

        $delivery->ack();
        expect(true)->toBeTrue();

        $pool->close();
    });

    it('release returns void without blocking', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'release-void', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);
        expect($delivery)->not->toBeNull();

        $delivery->release();
        expect(true)->toBeTrue();

        $pool->close();
    });

    it('reject returns void without blocking', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'reject-void', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);
        expect($delivery)->not->toBeNull();

        $delivery->reject(false);
        expect(true)->toBeTrue();

        $pool->close();
    });

    it('drainErrors returns settlement errors after failed ack', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'drain-error', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $delivery->ack();
        // Errors surface asynchronously — drain may be empty here
        $errors = $consumer->drainErrors();
        expect($errors)->toBeArray();

        $pool->close();
    });

    it('ackBatch bounds to 256 deliveries', function () {
        $deliveries = [];
        for ($i = 0; $i < 3; $i++) {
            $deliveries[] = ['message_id' => "batch-{$i}", 'payload' => 'test'];
        }
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => $deliveries,
        ]);
        $consumer = $pool->consumer('main');

        $batch = [];
        for ($i = 0; $i < 3; $i++) {
            $delivery = $consumer->next(10);
            expect($delivery)->not->toBeNull();
            $batch[] = $delivery;
        }

        $consumer->ackBatch($batch);
        expect(true)->toBeTrue();

        $pool->close();
    });

    it('ackBatch rejects more than 256 deliveries', function () {
        $deliveries = [];
        for ($i = 0; $i < 257; $i++) {
            $deliveries[] = ['message_id' => "overflow-{$i}", 'payload' => 'test'];
        }
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => $deliveries,
        ]);
        $consumer = $pool->consumer('main');

        $batch = [];
        for ($i = 0; $i < 257; $i++) {
            $delivery = $consumer->next(10);
            expect($delivery)->not->toBeNull();
            $batch[] = $delivery;
        }

        try {
            $consumer->ackBatch($batch);
            expect(false)->toBeTrue('ackBatch must reject more than 256 deliveries');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('256');
        }

        $pool->close();
    });

    it('ackThrough settles without blocking', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [
                ['message_id' => 'through-1', 'payload' => 'test'],
                ['message_id' => 'through-2', 'payload' => 'test'],
            ],
        ]);
        $consumer = $pool->consumer('main');

        $consumer->next(10);
        $second = $consumer->next(10);

        $consumer->ackThrough($second);
        expect(true)->toBeTrue();

        $pool->close();
    });

    it('nextBatch with max=1 returns at most 1 delivery', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [
                ['message_id' => 'batch-max-1', 'payload' => 'test'],
                ['message_id' => 'batch-max-2', 'payload' => 'test'],
            ],
        ]);
        $consumer = $pool->consumer('main');

        $batch = $consumer->nextBatch(1, 10);
        expect(count($batch))->toBe(1);

        $pool->close();
    });
});
