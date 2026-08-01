--TEST--
Rabbit RS reuses one process-local handle per normalized configuration
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

function config(string $vhost = '/'): array {
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => $vhost,
            'credentials' => ['username' => 'guest', 'password' => 'secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [],
        'topology_mode' => 'external',
    ];
}

$first = new Goopil\RabbitRs\Pool(config());
$second = new Goopil\RabbitRs\Pool(config());
$different = new Goopil\RabbitRs\Pool(config('/other'));
$firstStats = $first->stats();
$secondStats = $second->stats();
$differentStats = $different->stats();

expect_true($firstStats['pid'] === getmypid(), 'registry PID');
expect_true($firstStats['key'] === $secondStats['key'], 'equivalent configuration key');
expect_true($firstStats['handle'] === $secondStats['handle'], 'equivalent pools must share a handle');
expect_true($firstStats['key'] !== $differentStats['key'], 'different configuration key');
expect_true($firstStats['handle'] !== $differentStats['handle'], 'different pools need distinct handles');

$first->close();
try {
    $second->stats();
    throw new Exception('closing a shared handle must invalidate its aliases');
} catch (Goopil\RabbitRs\Exception $exception) {
    expect_true(str_contains($exception->getMessage(), 'closed'), 'shared close error');
}

$replacement = new Goopil\RabbitRs\Pool(config());
expect_true($replacement->stats()['handle'] !== $firstStats['handle'], 'closed handle replacement');
$replacement->close();
$different->close();

echo "OK\n";
?>
--EXPECT--
OK