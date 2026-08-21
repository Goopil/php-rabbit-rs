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
