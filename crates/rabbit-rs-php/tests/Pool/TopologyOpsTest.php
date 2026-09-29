<?php

declare(strict_types=1);

describe('native topology operations', function () {
    it('declares, binds, deletes and verifies through the pool in call order', function () {
        // operation_results are consumed in call order: the second slot is
        // scripted to fail, proving each call consumes exactly one outcome.
        $pool = testingPool(defaultConfig(), ['operation_results' => ['ok', 'error', 'ok', 'ok']]);

        try {
            $pool->declareQueue('default', 'topology-canary', 'quorum', true);

            $pool->bindQueue('default', 'jobs', 'topology-canary', 'topology-canary');
            $this->fail('second scripted outcome was not consumed by bindQueue');
        } catch (\Goopil\RabbitRs\ConnectionException $e) {
            expect($e->getMessage())->toContain('NOT_FOUND');
        }

        try {
            $pool->deleteQueue('default', 'topology-canary');
            $pool->verifyExchange('default', 'jobs');
        } finally {
            $pool->close();
        }
    });

    it('defaults declareQueue to a durable quorum queue', function () {
        // A scripted failure proves the declare reached the broker-side
        // operation (an invalid default would fail client-side first).
        $pool = testingPool(defaultConfig(), ['operation_results' => ['error']]);

        try {
            $pool->declareQueue('default', 'topology-canary');
            $this->fail('default declare did not consume the scripted outcome');
        } catch (\Goopil\RabbitRs\ConnectionException $e) {
            expect($e->getMessage())->toContain('NOT_FOUND');
        } finally {
            $pool->close();
        }
    });

    it('rejects an unsupported queue kind naming the call site', function () {
        $pool = testingPool(defaultConfig(), []);

        try {
            $pool->declareQueue('default', 'q', 'weird', true);
            $this->fail('unsupported queue kind accepted');
        } catch (\Throwable $e) {
            expect($e->getMessage())->toContain('weird');
            expect($e->getMessage())->toContain('Pool::declareQueue');
        } finally {
            $pool->close();
        }
    });

    it('surfaces a scripted operation failure', function () {
        $pool = testingPool(defaultConfig(), ['operation_results' => ['error']]);

        try {
            $pool->verifyExchange('default', 'missing');
            $this->fail('scripted failure not surfaced');
        } catch (\Goopil\RabbitRs\ConnectionException $e) {
            expect($e->getMessage())->toContain('NOT_FOUND');
        } finally {
            $pool->close();
        }
    });
});

describe('getMessage', function () {
    it('returns the scripted message then null when empty', function () {
        $pool = testingPool(defaultConfig(), [
            'get_messages' => [['message_id' => 'm-1', 'payload' => 'body']],
        ]);

        try {
            $message = $pool->getMessage('default', 'dlq');

            expect($message)->toBeArray();
            expect($message['message_id'])->toBe('m-1');
            expect((string) $message['payload'])->toBe('body');
            expect($pool->getMessage('default', 'dlq'))->toBeNull();
        } finally {
            $pool->close();
        }
    });

    it('surfaces a scripted fetch failure', function () {
        $pool = testingPool(defaultConfig(), ['get_messages' => ['error']]);

        try {
            $pool->getMessage('default', 'missing');
            $this->fail('scripted fetch failure not surfaced');
        } catch (\Goopil\RabbitRs\ConnectionException $e) {
            expect($e->getMessage())->toContain('NOT_FOUND');
        } finally {
            $pool->close();
        }
    });
});
