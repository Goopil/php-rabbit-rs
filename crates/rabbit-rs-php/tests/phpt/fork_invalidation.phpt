--TEST--
Rabbit RS invalidates inherited pools and creates a child-local registry after fork
--SKIPIF--
<?php
if (!extension_loaded('pcntl')) {
    die('skip pcntl is required');
}
?>
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

function config(): array {
    return [
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
    ];
}

$parentPool = new Goopil\RabbitRs\Pool(config());
$parentStats = $parentPool->stats();
$childPid = pcntl_fork();
if ($childPid === -1) {
    throw new Exception('fork failed');
}
if ($childPid === 0) {
    try {
        $parentPool->stats();
        exit(10);
    } catch (Goopil\RabbitRs\Exception $exception) {
        if (!str_contains($exception->getMessage(), 'fork')) {
            exit(11);
        }
    }

    $childPool = new Goopil\RabbitRs\Pool(config());
    $childStats = $childPool->stats();
    if ($childStats['pid'] !== getmypid()) {
        exit(12);
    }
    if ($childStats['handle'] === $parentStats['handle']) {
        exit(13);
    }
    $childPool->close();
    exit(0);
}

pcntl_waitpid($childPid, $status);
expect_true(pcntl_wifexited($status), 'child must exit normally');
expect_true(pcntl_wexitstatus($status) === 0, 'child lifecycle assertions');
expect_true($parentPool->stats()['handle'] === $parentStats['handle'], 'parent handle remains valid');
$parentPool->close();

echo "OK\n";
?>
--EXPECT--
OK