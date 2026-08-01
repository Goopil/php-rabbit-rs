--TEST--
Rabbit RS maps bounded publisher saturation to BackpressureException
--FILE--
<?php
function message(string $messageId): array {
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => 'payload',
        'message_id' => $messageId,
        'headers' => [
            'trace' => [
                'sampled' => true,
                'tags' => ['native', 1],
            ],
        ],
        'timeout_ms' => 1,
    ];
}

$pool = Goopil\RabbitRs\testing_pool([
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
], [
    'publisher_capacity' => 1,
    'pending_confirmations' => 1,
]);

try {
    $pool->publishBatch([message('first'), message('second')]);
    throw new Exception('bounded publisher must apply backpressure');
} catch (Goopil\RabbitRs\BackpressureException $exception) {
    if (!str_contains($exception->getMessage(), 'capacity')) {
        throw new Exception('backpressure error must explain capacity exhaustion');
    }
}

if ($pool->stats()['backpressure_total'] !== 1) {
    throw new Exception('backpressure metric must be incremented');
}

$pool->close();
echo "OK\n";
?>
--EXPECT--
OK