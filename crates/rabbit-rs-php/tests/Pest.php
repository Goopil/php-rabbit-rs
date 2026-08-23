<?php

declare(strict_types=1);

uses(\PHPUnit\Framework\TestCase::class)->in(__DIR__);

beforeAll(function () {
    if (!extension_loaded('rabbit_rs')) {
        test('extension loaded', fn () => true)->markTestSkipped(
            'rabbit_rs extension not loaded'
        );
        return;
    }
});

function testingPool(array $config, array $scenario): \Goopil\RabbitRs\Pool
{
    return \Goopil\RabbitRs\testing_pool($config, $scenario);
}

function defaultConfig(): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => '/',
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [],
        'topology_mode' => 'external',
    ];
}

function defaultConfigWithWorkers(): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => '/',
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [[
            'name' => 'main',
            'subscriptions' => [[
                'name' => 'default',
                'broker' => 'default',
                'queue' => 'jobs',
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => 512,
            ]],
            'scheduler' => [
                'strategy' => 'weighted_fair',
                'max_in_flight' => 512,
            ],
        ]],
        'topology_mode' => 'external',
    ];
}

function pubMessage(string $messageId, string $payload = 'payload', array $headers = [], int $timeoutMs = 1000): array
{
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => $payload,
        'message_id' => $messageId,
        'headers' => $headers,
        'timeout_ms' => $timeoutMs,
    ];
}
