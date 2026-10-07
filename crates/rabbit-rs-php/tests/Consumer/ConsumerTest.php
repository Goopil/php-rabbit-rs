<?php

declare(strict_types=1);

describe('delivery terminal state', function () {
    it('delivers binary-safe payloads with metadata', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [[
                'message_id' => 'delivery-1',
                'correlation_id' => 'trace-42',
                'payload' => "job\0payload\xff",
                'headers' => [
                    'trace' => "trace\0value",
                    'enabled' => true,
                    'count' => 42,
                    'ratio' => 1.5,
                    'nothing' => null,
                    'x-death' => [[
                        'queue' => 'jobs.dead',
                        'count' => 1,
                    ]],
                ],
                'attempts' => 2,
            ], [
                'message_id' => 'delivery-release',
                'payload' => 'release',
            ], [
                'message_id' => 'delivery-reject',
                'payload' => 'reject',
            ], [
                'message_id' => 'delivery-requeue',
                'payload' => 'requeue',
            ]],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\Delivery::class);
        expect($delivery->payload())->toBe("job\0payload\xff");

        $metadata = $delivery->metadata();
        expect($metadata['message_id'])->toBe('delivery-1');
        expect($metadata['correlation_id'])->toBe('trace-42');
        expect($metadata['attempts'])->toBe(2);
        expect($metadata['headers']['trace'])->toBe("trace\0value");
        expect($metadata['headers']['enabled'])->toBeTrue();
        expect($metadata['headers']['count'])->toBe(42);
        expect($metadata['headers']['ratio'])->toBe(1.5);
        expect($metadata['headers']['nothing'])->toBeNull();
        expect($metadata)->not->toHaveKey('x-death');
        expect($metadata['state'])->toBe('pending');

        $pool->close();
    });

    it('delivers empty and single-byte payloads', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [
                ['message_id' => 'delivery-empty', 'payload' => ''],
                ['message_id' => 'delivery-one-char', 'payload' => 'x'],
            ],
        ]);
        $consumer = $pool->consumer('main');
        $empty = $consumer->next(10);
        $oneChar = $consumer->next(10);

        expect($empty)->toBeInstanceOf(\Goopil\RabbitRs\Delivery::class);
        expect($empty->payload())->toBe('');
        expect($oneChar)->toBeInstanceOf(\Goopil\RabbitRs\Delivery::class);
        expect($oneChar->payload())->toBe('x');

        $pool->close();
    });

    it('makes ACK terminal and rejects a second ACK', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'ack-test', 'payload' => 'test']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $delivery->ack();
        expect($delivery->metadata()['state'])->toBe('acked');

        try {
            $delivery->ack();
            expect(false)->toBeTrue('a second ACK must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('terminal');
        }

        $pool->close();
    });

    it('makes release terminal', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'release-test', 'payload' => 'release']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $delivery->release();
        expect($delivery->metadata()['state'])->toBe('rejected');

        $pool->close();
    });

    it('makes reject(false) terminal', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'reject-test', 'payload' => 'reject']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $delivery->reject(false);
        expect($delivery->metadata()['state'])->toBe('rejected');

        $pool->close();
    });

    it('makes reject(true) terminal', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'requeue-test', 'payload' => 'requeue']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $delivery->reject(true);
        expect($delivery->metadata()['state'])->toBe('rejected');

        $pool->close();
    });

    it('fails operations after consumer close', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'close-test', 'payload' => 'close']],
        ]);
        $consumer = $pool->consumer('main');
        $consumer->close();

        try {
            $consumer->next(0);
            expect(false)->toBeTrue('operation after consumer close must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('closed');
        }

        $pool->close();
    });
});

describe('timeout ceiling', function () {
    it('rejects a next() timeout beyond the shared 24h ceiling', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'ceiling-over', 'payload' => 'payload']],
        ]);
        $consumer = $pool->consumer('main');

        try {
            $consumer->next(86_400_001);
            expect(false)->toBeTrue('a timeout beyond the 24h ceiling must throw a ValueError');
        } catch (\ValueError $e) {
            expect($e->getMessage())->toContain('timeoutMs: exceeds the 86400000 millisecond limit');
        }

        $pool->close();
    });

    it('accepts a next() timeout at the shared 24h ceiling', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'ceiling-max', 'payload' => 'payload']],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(86_400_000);

        expect($delivery)->toBeInstanceOf(\Goopil\RabbitRs\Delivery::class);
        expect($delivery->payload())->toBe('payload');

        $pool->close();
    });

    it('rejects a nextBatch() timeout beyond the shared 24h ceiling', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [['message_id' => 'batch-ceiling-over', 'payload' => 'payload']],
        ]);
        $consumer = $pool->consumer('main');

        try {
            $consumer->nextBatch(1, 86_400_001);
            expect(false)->toBeTrue('a timeout beyond the 24h ceiling must throw a ValueError');
        } catch (\ValueError $e) {
            expect($e->getMessage())->toContain('timeoutMs: exceeds the 86400000 millisecond limit');
        }

        $pool->close();
    });
});
