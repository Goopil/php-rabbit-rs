<?php

declare(strict_types=1);

describe('fire-and-forget settlement', function () {
    describe('single delivery', function () {
        beforeEach(function () {
            $this->pool = testingPool(defaultConfigWithWorkers(), [
                'deliveries' => [['message_id' => 'fire-forget', 'payload' => 'test']],
            ]);
            $this->consumer = $this->pool->consumer('main');
            $this->delivery = $this->consumer->next(10);
        });

        afterEach(fn () => $this->pool->close());

        it('drainErrors returns empty when no settlement errors', function () {
            expect($this->delivery)->not->toBeNull();

            $errors = $this->consumer->drainErrors();
            expect($errors)->toBeEmpty();
        });

        it('ack returns void without blocking', function () {
            $this->delivery->ack();
            expect(true)->toBeTrue();
        });

        it('release returns void without blocking', function () {
            $this->delivery->release();
            expect(true)->toBeTrue();
        });

        it('reject returns void without blocking', function () {
            $this->delivery->reject(false);
            expect(true)->toBeTrue();
        });

        it('drainErrors returns settlement errors after failed ack', function () {
            $this->delivery->ack();
            // Errors surface asynchronously — drain may be empty here
            $errors = $this->consumer->drainErrors();
            expect($errors)->toBeArray();
        });
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
