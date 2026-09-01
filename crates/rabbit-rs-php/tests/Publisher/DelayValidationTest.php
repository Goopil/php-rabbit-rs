<?php

declare(strict_types=1);

/**
 * Issue #73 (audit F-06): a delay beyond the largest configured TTL bucket
 * must be refused with a typed error at the conversion boundary — never
 * published immediately with an x-delay header a normal exchange ignores.
 * Plugin mode accepts any delay.
 */

function ttlDelayConfig(): array
{
    return [
        ...defaultConfig(),
        'delay' => [
            'mode' => 'ttl',
            'buckets' => [1, 5, 30],
            'max_buckets' => 8,
            'queue_expiry_margin' => 60,
        ],
    ];
}

function pubMessageWithDelay(string $messageId, int $delayMs): array
{
    return [...pubMessage($messageId), 'delay_ms' => $delayMs];
}

describe('delay validation', function () {
    it('rejects a delay beyond the largest TTL bucket naming the limit', function (): void {
        $pool = testingPool(ttlDelayConfig(), ['confirmed_publications' => 1]);

        try {
            $pool->publish(pubMessageWithDelay('too-late', 60_000));
            expect(false)->toBeTrue('the oversized delay must be refused');
        } catch (\Goopil\RabbitRs\Exception $exception) {
            expect($exception->getMessage())->toContain('message.delay_ms')
                ->and($exception->getMessage())->toContain('30000');
        } finally {
            $pool->close();
        }
    });

    it('accepts a delay equal to the largest TTL bucket', function (): void {
        $pool = testingPool(ttlDelayConfig(), ['confirmed_publications' => 1]);

        try {
            expect($pool->publish(pubMessageWithDelay('boundary-job', 30_000)))
                ->toBe('boundary-job');
        } finally {
            $pool->close();
        }
    });

    it('accepts any delay in plugin mode', function (): void {
        $pool = testingPool(defaultConfig(), ['confirmed_publications' => 1]);

        try {
            expect($pool->publish(pubMessageWithDelay('plugin-job', 3_600_000)))
                ->toBe('plugin-job');
        } finally {
            $pool->close();
        }
    });
});
