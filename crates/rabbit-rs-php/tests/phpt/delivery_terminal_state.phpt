--TEST--
Rabbit RS deliveries expose binary data and reject a second terminal transition
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

$config = [
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
        'max_in_flight' => 1,
        'scheduler' => ['strategy' => 'weighted_fair'],
    ]],
    'topology_mode' => 'external',
];

$pool = Goopil\RabbitRs\testing_pool($config, [
    'deliveries' => [[
        'message_id' => 'delivery-1',
        'payload' => "job\0payload\xff",
        'headers' => ['trace' => "trace\0value"],
        'attempts' => 2,
    ]],
]);
$consumer = $pool->consumer('main');
$delivery = $consumer->next(10);

expect_true($delivery instanceof Goopil\RabbitRs\Delivery, 'fixture must deliver one message');
expect_true($delivery->payload() === "job\0payload\xff", 'payload must remain binary-safe');
$metadata = $delivery->metadata();
expect_true($metadata['message_id'] !== '', 'message id metadata');
expect_true($metadata['attempts'] === 2, 'attempt count metadata');
expect_true($metadata['headers']['trace'] === "trace\0value", 'binary header metadata');
expect_true($metadata['state'] === 'pending', 'initial delivery state');

$delivery->ack();
expect_true($delivery->metadata()['state'] === 'acked', 'ACK must be terminal');
try {
    $delivery->ack();
    throw new Exception('a second ACK must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'terminal'), 'double ACK error');
}

$consumer->close();
try {
    $consumer->next(0);
    throw new Exception('operation after consumer close must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'closed'), 'closed consumer error');
}

$pool->close();
echo "OK\n";
?>
--EXPECT--
OK