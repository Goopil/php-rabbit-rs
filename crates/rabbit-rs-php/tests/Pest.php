<?php

declare(strict_types=1);

uses(\PHPUnit\Framework\TestCase::class)->in(__DIR__);

beforeAll(function () {
    if (!extension_loaded('rabbit_rs')) {
        test('extension loaded', fn () => true)->markTestSkipped(
            'rabbit_rs extension not loaded'
        );
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
            'tls' => ['enabled' => false],
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
            'tls' => ['enabled' => false],
            'heartbeat' => 30,
        ]],
        'workers' => [[
            'name' => 'main',
            'subscriptions' => [[
                'name' => 'default',
                'broker' => 'default',
                'queue' => 'jobs',
                'weight' => 1,
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

// The default per-message deadline is generous (issue #189): on a loaded
// Docker CI runner the mock confirmations can take over a second to drain,
// and an expired deadline resolves the publication with a terminal timeout
// error instead of the scripted outcome. Tests that exercise deadline expiry
// pass an explicit short timeoutMs.
function pubMessage(string $messageId, string $payload = 'payload', array $headers = [], int $timeoutMs = 5000): array
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
