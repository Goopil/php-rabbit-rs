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
            'trace.sampled' => true,
            'trace.source' => 'native',
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
    'workers' => [[
        'name' => 'main',
        'subscriptions' => [[
            'name' => 'default',
            'broker' => 'default',
            'queue' => 'jobs',
            'weight' => 1,
            'priority_class' => 0,
            'prefetch' => 1,
        ]],
        'scheduler' => [
            'strategy' => 'weighted_fair',
            'max_in_flight' => 1,
        ],
    ]],
    'topology_mode' => 'external',
], [
    'publisher_capacity' => 1,
    'pending_confirmations' => 1,
]);
$consumer = $pool->consumer('main');

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
try {
    $consumer->next(1);
    throw new Exception('pool close must terminate its active consumer');
} catch (Goopil\RabbitRs\Exception $exception) {
    if (!str_contains($exception->getMessage(), 'closed')) {
        throw new Exception('consumer close must be explicit');
    }
}
echo "OK\n";
?>
--EXPECT--
OK