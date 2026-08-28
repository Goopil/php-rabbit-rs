<?php

declare(strict_types=1);

describe('binary payload validation', function () {
    it('passes binary payloads through to the transport layer', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        $binaryMessage = [
            'broker' => 'missing',
            'exchange' => 'jobs',
            'routing_key' => 'default',
            'payload' => "before\0after\xff",
            'message_id' => 'binary-message',
            'headers' => ['binary' => "header\0value"],
            'timeout_ms' => 100,
        ];

        try {
            $pool->publish($binaryMessage);
            $pool->flush();
            expect(false)->toBeTrue('unknown broker must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('brokers.missing');
        }

        $pool->close();
    });

    it('rejects oversized payload', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        $oversized = [
            'broker' => 'default',
            'exchange' => 'jobs',
            'routing_key' => 'default',
            'payload' => str_repeat('x', 1024 * 1024 + 1),
            'message_id' => 'oversized',
            'timeout_ms' => 100,
        ];

        try {
            $pool->publish($oversized);
            expect(false)->toBeTrue('oversized payload must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('message.payload');
        }

        $pool->close();
    });

    it('rejects resource headers', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        $message = [
            'broker' => 'default',
            'exchange' => 'jobs',
            'routing_key' => 'default',
            'payload' => 'test',
            'message_id' => 'resource-header',
            'headers' => ['resource' => fopen('php://memory', 'r')],
            'timeout_ms' => 100,
        ];

        try {
            $pool->publish($message);
            expect(false)->toBeTrue('resource header must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('message.headers.resource');
        }

        $pool->close();
    });

    it('rejects object headers', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        $message = [
            'broker' => 'default',
            'exchange' => 'jobs',
            'routing_key' => 'default',
            'payload' => 'test',
            'message_id' => 'object-header',
            'headers' => ['object' => new \stdClass()],
            'timeout_ms' => 100,
        ];

        try {
            $pool->publish($message);
            expect(false)->toBeTrue('object header must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('message.headers.object');
        }

        $pool->close();
    });

    it('rejects recursive headers', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        $recursive = [];
        $recursive['self'] = &$recursive;
        $message = [
            'broker' => 'default',
            'exchange' => 'jobs',
            'routing_key' => 'default',
            'payload' => 'test',
            'message_id' => 'recursive-header',
            'headers' => ['recursive' => &$recursive],
            'timeout_ms' => 100,
        ];

        try {
            $pool->publish($message);
            expect(false)->toBeTrue('recursive header must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('headers must be flat');
        }

        unset($message, $recursive);
        $pool->close();
    });

    it('rejects invalid batch items', function () {
        $pool = new \Goopil\RabbitRs\Pool(defaultConfig());

        try {
            $pool->publishBatch(['not a message']);
            expect(false)->toBeTrue('invalid batch item must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('messages[0]');
        }

        $pool->close();
    });
});
