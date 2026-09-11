<?php
declare(strict_types=1);

// Broker-side queue depth observer: reads the queue's message count through
// an AMQP passive declare (data plane). The lab's management API cannot be
// used for depth assertions: the lab ships with
// `management.disable_metrics_collector = true`, so queue stats are never
// reported there. This probe is independent of the publish path under test:
// it opens its own pool, holds no publications, and only reads.

$queue = $_SERVER['RABBIT_RS_QUEUE'] ?? ($argv[1] ?? '');
if ($queue === '') {
    fwrite(STDERR, "RABBIT_RS_QUEUE is required\n");
    exit(1);
}

// getenv() reads FastCGI params under php-fpm and the process environment
// under CLI, so this observer works in both modes.
$brokerHost = getenv('RABBIT_RS_BROKER_HOST') ?: '127.0.0.1';
$brokerPort = (int) (getenv('RABBIT_RS_BROKER_PORT') ?: '5672');

$config = [
    'brokers' => [[
        'name' => 'default',
        'hosts' => [['host' => $brokerHost, 'port' => $brokerPort]],
        'vhost' => '/',
        'credentials' => ['username' => 'rabbit_rs', 'password' => 'rabbit_rs_lab'],
        'tls' => ['enabled' => false],
        'heartbeat' => 30,
    ]],
    'workers' => [],
    'topology_mode' => 'external',
];

$pool = new Goopil\RabbitRs\Pool($config);
$depth = $pool->size('default', $queue);
$pool->close();

header('Content-Type: application/json');
echo json_encode([
    'pid' => getmypid(),
    'queue' => $queue,
    'depth' => $depth,
], JSON_THROW_ON_ERROR);
