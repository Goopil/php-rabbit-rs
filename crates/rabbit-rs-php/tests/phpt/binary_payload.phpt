--TEST--
Rabbit RS validates binary publications before touching the network
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

$pool = new Goopil\RabbitRs\Pool([
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
]);

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
    throw new Exception('unknown broker must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'brokers.missing'), 'binary payload must pass conversion');
}

$oversized = $binaryMessage;
$oversized['payload'] = str_repeat('x', 1024 * 1024 + 1);
try {
    $pool->publish($oversized);
    throw new Exception('oversized payload must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'message.payload'), 'payload size path');
}

$invalidHeader = $binaryMessage;
$invalidHeader['headers']['resource'] = fopen('php://memory', 'r');
try {
    $pool->publish($invalidHeader);
    throw new Exception('resource header must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'message.headers.resource'), 'header path');
}

$invalidHeader = $binaryMessage;
$invalidHeader['headers']['object'] = new stdClass();
try {
    $pool->publish($invalidHeader);
    throw new Exception('object header must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'message.headers.object'), 'object header path');
}

$recursive = [];
$recursive['self'] = &$recursive;
$invalidHeader = $binaryMessage;
$invalidHeader['headers']['recursive'] = &$recursive;
try {
    $pool->publish($invalidHeader);
    throw new Exception('recursive header must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'recursive arrays'), 'recursive header error');
}
unset($invalidHeader, $recursive);

try {
    $pool->publishBatch(['not a message']);
    throw new Exception('invalid batch item must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'messages.0'), 'batch item path');
}

$pool->close();
echo "OK\n";
?>
--EXPECT--
OK
