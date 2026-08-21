<?php

declare(strict_types=1);

function boundaryMessage(string $id, string $payload = 'x', array $headers = [], int $timeoutMs = 1000): array
{
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => $payload,
        'message_id' => $id,
        'headers' => $headers,
        'timeout_ms' => $timeoutMs,
    ];
}

function expectBoundaryError(callable $operation, string $path): void
{
    try {
        $operation();
        expect(false)->toBeTrue("expected failure at {$path}");
    } catch (\Goopil\RabbitRs\Exception $e) {
        expect($e->getMessage())->toContain($path);
    }
}

describe('boundary limits', function () {
    beforeEach(function () {
        $this->pool = testingPool(defaultConfig(), ['confirmed_publications' => 265]);
    });

    afterEach(function () {
        $this->pool->close();
    });

    it('accepts a batch of 256 messages but rejects 257', function () {
        $maxBatch = [];
        for ($i = 0; $i < 256; $i++) {
            $maxBatch[] = boundaryMessage("batch-{$i}");
        }
        expect(count($this->pool->publishBatch($maxBatch)))->toBe(256);

        expectBoundaryError(
            fn () => $this->pool->publishBatch([...$maxBatch, boundaryMessage('batch-256')]),
            'messages: exceeds the 256 message limit',
        );
    });

    it('enforces cumulative 1 MiB payload limit', function () {
        $half = str_repeat('p', 512 * 1024);
        $this->pool->publishBatch([boundaryMessage('payload-a', $half), boundaryMessage('payload-b', $half)]);

        expectBoundaryError(
            fn () => $this->pool->publishBatch([boundaryMessage('payload-c', $half), boundaryMessage('payload-d', $half . 'x')]),
            'messages[1].payload',
        );
    });

    it('enforces 128 header count limit', function () {
        $maxHeaders = [];
        for ($i = 0; $i < 128; $i++) {
            $maxHeaders["h{$i}"] = $i;
        }
        $this->pool->publish(boundaryMessage('headers-128', headers: $maxHeaders));

        $maxHeaders['overflow'] = true;
        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('headers-129', headers: $maxHeaders)),
            'message.headers',
        );
    });

    it('enforces cumulative header count across batch', function () {
        $halfHeaders = [];
        for ($i = 0; $i < 64; $i++) {
            $halfHeaders["batch-h{$i}"] = true;
        }
        $this->pool->publishBatch([
            boundaryMessage('batch-headers-a', headers: $halfHeaders),
            boundaryMessage('batch-headers-b', headers: $halfHeaders),
        ]);

        $overflowHeaders = $halfHeaders;
        $overflowHeaders['overflow'] = true;
        expectBoundaryError(
            fn () => $this->pool->publishBatch([
                boundaryMessage('batch-headers-c', headers: $halfHeaders),
                boundaryMessage('batch-headers-d', headers: $overflowHeaders),
            ]),
            'messages[1].headers',
        );
    });

    it('enforces 64 KiB header byte size limit', function () {
        $this->pool->publish(boundaryMessage('header-bytes-max', headers: ['h' => str_repeat('h', 64 * 1024 - 1)]));

        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('header-bytes-over', headers: ['h' => str_repeat('h', 64 * 1024)])),
            'message.headers.h',
        );
    });

    it('enforces 64 KiB header key size limit', function () {
        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('header-key-over', headers: [str_repeat('k', 64 * 1024 + 1) => null])),
            'message.headers',
        );
    });

    it('enforces cumulative header byte size across batch', function () {
        $this->pool->publishBatch([
            boundaryMessage('batch-header-bytes-a', headers: ['a' => str_repeat('a', 32 * 1024 - 1)]),
            boundaryMessage('batch-header-bytes-b', headers: ['b' => str_repeat('b', 32 * 1024 - 1)]),
        ]);

        expectBoundaryError(
            fn () => $this->pool->publishBatch([
                boundaryMessage('batch-header-bytes-c', headers: ['a' => str_repeat('a', 32 * 1024 - 1)]),
                boundaryMessage('batch-header-bytes-d', headers: ['b' => str_repeat('b', 32 * 1024)]),
            ]),
            'messages[1].headers.b',
        );
    });

    it('rejects nested headers', function () {
        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('nested-header', headers: ['trace_id' => ['nested']])),
            'message.headers.trace_id',
        );
    });

    it('rejects integer header keys', function () {
        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('integer-header-key', headers: [0 => 'invalid'])),
            'message.headers.0',
        );
    });

    it('rejects invalid batch items', function () {
        expectBoundaryError(
            fn () => $this->pool->publishBatch(['not-a-message']),
            'messages[0]',
        );
    });

    it('enforces timeout range 1 to 86,400,000 ms', function () {
        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('timeout-zero', timeoutMs: 0)),
            'message.timeout_ms',
        );

        $this->pool->publish(boundaryMessage('timeout-max', timeoutMs: 86_400_000));

        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('timeout-over', timeoutMs: 86_400_001)),
            'message.timeout_ms',
        );

        expectBoundaryError(
            fn () => $this->pool->publish(boundaryMessage('timeout-int-max', timeoutMs: PHP_INT_MAX)),
            'message.timeout_ms',
        );
    });
});
