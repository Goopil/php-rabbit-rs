<?php
declare(strict_types=1);

$config = [
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

$first = new Goopil\RabbitRs\Pool($config);
$second = new Goopil\RabbitRs\Pool($config);
usleep(20_000);

header('Content-Type: application/json');
echo json_encode([
    'pid' => getmypid(),
    'first_handle' => $first->stats()['handle'],
    'second_handle' => $second->stats()['handle'],
], JSON_THROW_ON_ERROR);
