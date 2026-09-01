--TEST--
Publishing boundary limits (batch, payload, headers, timeouts)
--FILE--
<?php
function config(): array {
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

function message(string $id, string $payload = 'x', array $headers = [], int $timeoutMs = 1000): array {
    return [
        'broker' => 'default',
        'exchange' => 'jobs',
        'routing_key' => 'default',
        'payload' => $payload,
        'message_id' => $id,
        'headers' => $headers,
        'timeout_ms' => $timeoutMs,
    ];
}

function expect_error(callable $operation, string $path): void {
    try {
        $operation();
        throw new Exception("expected failure at {$path}");
    } catch (Goopil\RabbitRs\Exception $exception) {
        if (!str_contains($exception->getMessage(), $path)) {
            throw new Exception("missing error path {$path}: {$exception->getMessage()}");
        }
    }
}

$pool = Goopil\RabbitRs\testing_pool(config(), ['confirmed_publications' => 265]);

$maxBatch = [];
for ($index = 0; $index < 256; $index++) {
    $maxBatch[] = message("batch-{$index}");
}
if (count($pool->publishBatch($maxBatch)) !== 256) {
    throw new Exception('a batch of 256 messages must be accepted');
}
expect_error(
    fn () => $pool->publishBatch([...$maxBatch, message('batch-256')]),
    'messages: exceeds the 256 message limit',
);

$half = str_repeat('p', 512 * 1024);
$pool->publishBatch([message('payload-a', $half), message('payload-b', $half)]);
expect_error(
    fn () => $pool->publishBatch([message('payload-c', $half), message('payload-d', $half . 'x')]),
    'messages[1].payload',
);

$maxHeaders = [];
for ($index = 0; $index < 128; $index++) {
    $maxHeaders["h{$index}"] = $index;
}
$pool->publish(message('headers-128', headers: $maxHeaders));
$maxHeaders['overflow'] = true;
expect_error(fn () => $pool->publish(message('headers-129', headers: $maxHeaders)), 'message.headers');

$halfHeaders = [];
for ($index = 0; $index < 64; $index++) {
    $halfHeaders["batch-h{$index}"] = true;
}
$pool->publishBatch([
    message('batch-headers-a', headers: $halfHeaders),
    message('batch-headers-b', headers: $halfHeaders),
]);
$overflowHeaders = $halfHeaders;
$overflowHeaders['overflow'] = true;
expect_error(
    fn () => $pool->publishBatch([
        message('batch-headers-c', headers: $halfHeaders),
        message('batch-headers-d', headers: $overflowHeaders),
    ]),
    'messages[1].headers',
);

$pool->publish(message('header-bytes-max', headers: ['h' => str_repeat('h', 64 * 1024 - 1)]));
expect_error(
    fn () => $pool->publish(message('header-bytes-over', headers: ['h' => str_repeat('h', 64 * 1024)])),
    'message.headers.h',
);
expect_error(
    fn () => $pool->publish(message('header-key-over', headers: [str_repeat('k', 64 * 1024 + 1) => null])),
    'message.headers',
);
$pool->publishBatch([
    message('batch-header-bytes-a', headers: ['a' => str_repeat('a', 32 * 1024 - 1)]),
    message('batch-header-bytes-b', headers: ['b' => str_repeat('b', 32 * 1024 - 1)]),
]);
expect_error(
    fn () => $pool->publishBatch([
        message('batch-header-bytes-c', headers: ['a' => str_repeat('a', 32 * 1024 - 1)]),
        message('batch-header-bytes-d', headers: ['b' => str_repeat('b', 32 * 1024)]),
    ]),
    'messages[1].headers.b',
);
expect_error(
    fn () => $pool->publish(message('nested-header', headers: ['trace_id' => ['nested']])),
    'message.headers.trace_id',
);
expect_error(
    fn () => $pool->publish(message('integer-header-key', headers: [0 => 'invalid'])),
    'message.headers.0',
);

expect_error(fn () => $pool->publishBatch(['not-a-message']), 'messages[0]');
expect_error(fn () => $pool->publish(message('timeout-zero', timeoutMs: 0)), 'message.timeout_ms');
$pool->publish(message('timeout-max', timeoutMs: 86_400_000));
expect_error(
    fn () => $pool->publish(message('timeout-over', timeoutMs: 86_400_001)),
    'message.timeout_ms',
);
expect_error(
    fn () => $pool->publish(message('timeout-int-max', timeoutMs: PHP_INT_MAX)),
    'message.timeout_ms',
);

$pool->close();
echo "OK\n";
?>
--EXPECT--
OK
