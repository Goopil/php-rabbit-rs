<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\MessageMapper;
use Illuminate\Support\Str;

function mapperRoute(): array
{
    return [
        'broker' => 'default',
        'exchange' => 'laravel.jobs',
        'routing_key' => '{queue}',
    ];
}

describe('MessageMapper', function () {
    it('includes timeout_ms from publisher config by default', function () {
        $mapper = new MessageMapper(['confirm_timeout' => 5000]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            mapperRoute(),
            'orders',
        );

        expect($message['timeout_ms'])->toBe(5000);
    });

    it('uses explicit timeout_ms over the config default', function () {
        $mapper = new MessageMapper(['confirm_timeout' => 5000]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            mapperRoute(),
            'orders',
            ['timeout_ms' => 12000],
        );

        expect($message['timeout_ms'])->toBe(12000);
    });

    it('omits timeout_ms when config has no confirm_timeout', function () {
        $mapper = new MessageMapper([]);

        $message = $mapper->map(
            '{"job":"App\\\\Jobs\\\\Example"}',
            mapperRoute(),
            'orders',
        );

        expect($message)->not->toHaveKey('timeout_ms');
    });

    it('preserves all other fields', function () {
        $mapper = new MessageMapper(['confirm_timeout' => 30000]);

        $message = $mapper->map(
            'payload',
            mapperRoute(),
            'orders',
            ['content_type' => 'application/json', 'headers' => ['x-foo' => 'bar']],
            5000,
        );

        expect($message['broker'])->toBe('default')
            ->and($message['exchange'])->toBe('laravel.jobs')
            ->and($message['routing_key'])->toBe('orders')
            ->and($message['payload'])->toBe('payload')
            ->and(Str::isUuid($message['message_id']))->toBeTrue()
            ->and($message['content_type'])->toBe('application/json')
            ->and($message['headers'])->toBe(['x-foo' => 'bar'])
            ->and($message['delay_ms'])->toBe(5000)
            ->and($message['timeout_ms'])->toBe(30000);
    });
});
