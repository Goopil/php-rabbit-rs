--TEST--
Publication outcomes (ack, mandatory return, timeout, transport error)
--FILE--
<?php
function message(string $id, int $timeoutMs = 1000): array {
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => 'payload',
        'message_id' => $id,
        'timeout_ms' => $timeoutMs,
    ];
}

$pool = Goopil\RabbitRs\testing_pool([
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
], [
    'publication_outcomes' => ['ack', 'returned', 'pending', 'transport_error'],
]);

if ($pool->publish(message('confirmed')) !== 'confirmed') {
    throw new Exception('ACK must return the stable message id');
}

try {
    $pool->publish(message('returned'));
    $pool->flush();
    throw new Exception('mandatory return must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    if (!str_contains($exception->getMessage(), 'returned') || !str_contains($exception->getMessage(), '312')) {
        throw new Exception('mandatory return must retain its AMQP context');
    }
}

try {
    $pool->publish(message('timeout', 1));
    $pool->flush();
    throw new Exception('pending confirmation must time out');
} catch (Goopil\RabbitRs\Exception $exception) {
    if (!str_contains($exception->getMessage(), 'timed out')) {
        throw new Exception('confirmation timeout must be explicit');
    }
}

try {
    $pool->publish(message('transport'));
    $pool->flush();
    throw new Exception('transport failure must fail publication');
} catch (Goopil\RabbitRs\ConnectionException $exception) {
    if (!str_contains($exception->getMessage(), 'transport failed')) {
        throw new Exception('connection error must retain transport context');
    }
}

$pool->close();
echo "OK\n";
?>
--EXPECT--
OK
