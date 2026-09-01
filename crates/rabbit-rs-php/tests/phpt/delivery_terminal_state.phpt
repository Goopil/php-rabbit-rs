--TEST--
Delivery terminal states and binary-safe metadata
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
            'priority_class' => 0,
            'prefetch' => 1,
        ]],
        'scheduler' => [
            'strategy' => 'weighted_fair',
            'max_in_flight' => 1,
        ],
    ]],
    'topology_mode' => 'external',
];

$pool = Goopil\RabbitRs\testing_pool($config, [
    'deliveries' => [[
        'message_id' => 'delivery-1',
        'correlation_id' => 'trace-42',
        'payload' => "job\0payload\xff",
        'headers' => [
            'trace' => "trace\0value",
            'enabled' => true,
            'count' => 42,
            'ratio' => 1.5,
            'nothing' => null,
            'x-death' => [[
                'queue' => 'jobs.dead',
                'count' => 1,
            ]],
        ],
        'attempts' => 2,
    ], [
        'message_id' => 'delivery-release',
        'payload' => 'release',
    ], [
        'message_id' => 'delivery-reject',
        'payload' => 'reject',
    ], [
        'message_id' => 'delivery-requeue',
        'payload' => 'requeue',
    ]],
]);
$consumer = $pool->consumer('main');
$delivery = $consumer->next(10);

expect_true($delivery instanceof Goopil\RabbitRs\Delivery, 'fixture must deliver one message');
expect_true($delivery->payload() === "job\0payload\xff", 'payload must remain binary-safe');
$metadata = $delivery->metadata();
expect_true($metadata['message_id'] === 'delivery-1', 'broker message id metadata');
expect_true($metadata['correlation_id'] === 'trace-42', 'correlation id metadata');
expect_true($metadata['attempts'] === 2, 'attempt count metadata');
expect_true($metadata['headers']['trace'] === "trace\0value", 'binary header metadata');
expect_true($metadata['headers']['enabled'] === true, 'boolean header metadata');
expect_true($metadata['headers']['count'] === 42, 'integer header metadata');
expect_true($metadata['headers']['ratio'] === 1.5, 'floating-point header metadata');
expect_true($metadata['headers']['nothing'] === null, 'null header metadata');
expect_true(!array_key_exists('x-death', $metadata['headers']), 'nested broker headers are omitted');
expect_true($metadata['state'] === 'pending', 'initial delivery state');

$delivery->ack();
expect_true($delivery->metadata()['state'] === 'acked', 'ACK must be terminal');
try {
    $delivery->ack();
    throw new Exception('a second ACK must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'terminal'), 'double ACK error');
}

$released = $consumer->next(10);
$released->release();
expect_true($released->metadata()['state'] === 'rejected', 'release must be terminal');

$rejected = $consumer->next(10);
$rejected->reject(false);
expect_true($rejected->metadata()['state'] === 'rejected', 'reject(false) must be terminal');

$requeued = $consumer->next(10);
$requeued->reject(true);
expect_true($requeued->metadata()['state'] === 'rejected', 'reject(true) must be terminal');

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
