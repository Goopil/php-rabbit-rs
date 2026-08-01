--TEST--
Rabbit RS validates configuration and owns a process-local pool handle
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

function valid_config(): array {
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [[
                'host' => '127.0.0.1',
                'port' => 5672,
            ]],
            'vhost' => '/',
            'credentials' => [
                'username' => 'guest',
                'password' => 'native-password-must-stay-secret',
            ],
            'tls' => [
                'enabled' => false,
                'server_name' => null,
            ],
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
                'prefetch' => 16,
            ]],
            'scheduler' => [
                'strategy' => 'weighted_fair',
                'max_in_flight' => 64,
            ],
        ]],
        'topology_mode' => 'external',
    ];
}

$pool = new Goopil\RabbitRs\Pool(valid_config());
$stats = $pool->stats();
expect_true($stats['closed'] === false, 'new pool must be open');
expect_true($stats['pid'] === getmypid(), 'pool must belong to the current process');
expect_true(!array_key_exists('key', $stats), 'internal pool key must not be exposed');
expect_true(!str_contains(json_encode($stats), 'native-password-must-stay-secret'), 'stats must not expose credentials');

$invalid = valid_config();
$invalid['workers'][0]['subscriptions'][0]['prefetch'] = 0;
try {
    new Goopil\RabbitRs\Pool($invalid);
    throw new Exception('zero prefetch must be rejected');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(
        str_contains($exception->getMessage(), 'workers.main.subscriptions.default.prefetch'),
        'validation error must contain the exact path',
    );
    expect_true(
        !str_contains($exception->getMessage(), 'native-password-must-stay-secret'),
        'validation error must not expose credentials',
    );
}

$legacy = valid_config();
$legacy['workers'][0]['max_in_flight'] = 64;
unset($legacy['workers'][0]['scheduler']['max_in_flight']);
try {
    new Goopil\RabbitRs\Pool($legacy);
    throw new Exception('legacy max_in_flight path must be rejected');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(
        str_contains($exception->getMessage(), 'workers.main.max_in_flight')
        && str_contains($exception->getMessage(), 'workers.main.scheduler.max_in_flight'),
        'legacy configuration error must identify the canonical path',
    );
}

$recursive = [];
$recursive['self'] = &$recursive;
try {
    new Goopil\RabbitRs\Pool($recursive);
    throw new Exception('recursive configuration must be rejected');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'recursive'), 'recursive error');
}

$resourceConfig = valid_config();
$resourceConfig['unexpected'] = fopen('php://memory', 'r');
try {
    new Goopil\RabbitRs\Pool($resourceConfig);
    throw new Exception('resource configuration must be rejected');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'unexpected'), 'resource error path');
}

$pool->close();
$pool->close();
try {
    $pool->stats();
    throw new Exception('operation after close must fail');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'closed'), 'closed pool error');
}

echo "OK\n";
?>
--EXPECT--
OK
