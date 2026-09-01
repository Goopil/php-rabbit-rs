<?php

declare(strict_types=1);

describe('nested header round-trip', function () {
    it('exposes AMQP Array and Table headers as nested PHP arrays', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'deliveries' => [[
                'message_id' => 'nested-headers',
                'payload' => 'p',
                'headers' => [
                    'x-death' => [
                        'count' => 2,
                        'reason' => 'expired',
                        'queue' => 'jobs.dead',
                    ],
                    'history' => ['first', 'second'],
                    'flat' => 'value',
                ],
            ]],
        ]);
        $consumer = $pool->consumer('main');
        $delivery = $consumer->next(10);

        $headers = $delivery->metadata()['headers'];

        expect($headers['flat'])->toBe('value');
        expect($headers['x-death']['count'])->toBe(2);
        expect($headers['x-death']['reason'])->toBe('expired');
        expect($headers['x-death']['queue'])->toBe('jobs.dead');
        expect($headers['history'][0])->toBe('first');
        expect($headers['history'][1])->toBe('second');

        $pool->close();
    });
});
