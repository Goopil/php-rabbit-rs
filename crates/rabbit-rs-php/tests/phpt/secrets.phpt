--TEST--
Rabbit RS validation failures never expose configuration secrets
--FILE--
<?php
function expect_redacted(callable $operation, array $secrets): void {
    try {
        $operation();
        throw new Exception('operation must fail');
    } catch (Goopil\RabbitRs\Exception $exception) {
        foreach ($secrets as $secret) {
            if (str_contains($exception->getMessage(), $secret)) {
                throw new Exception('exception exposed a secret');
            }
        }
    }
}

$password = 'native-password-must-stay-secret';
$privateKey = 'PRIVATE-KEY-MATERIAL';
$config = [
    'brokers' => [[
        'name' => 'default',
        'hosts' => [['host' => '127.0.0.1', 'port' => 'not-a-port']],
        'vhost' => '/',
        'credentials' => ['username' => 'guest', 'password' => $password],
        'tls' => [
            'enabled' => true,
            'server_name' => 'rabbit.internal',
            'private_key' => $privateKey,
        ],
        'heartbeat' => 30,
    ]],
    'workers' => [],
    'topology_mode' => 'external',
];

expect_redacted(
    static fn() => new Goopil\RabbitRs\Pool($config),
    [$password, $privateKey],
);

echo "OK\n";
?>
--EXPECT--
OK