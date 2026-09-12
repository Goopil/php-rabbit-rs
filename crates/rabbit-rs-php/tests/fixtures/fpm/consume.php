<?php
declare(strict_types=1);

// FPM-resident single-delivery consumer fixture: pops one delivery, acks it,
// and returns its payload and message id.

$queue = $_SERVER['RABBIT_RS_QUEUE'] ?? '';
if ($queue === '') {
    fwrite(STDERR, "RABBIT_RS_QUEUE is required\n");
    exit(1);
}

$brokerHost = $_SERVER['RABBIT_RS_BROKER_HOST'] ?? '127.0.0.1';
$brokerPort = (int) ($_SERVER['RABBIT_RS_BROKER_PORT'] ?? '5672');
$timeoutMs = (int) ($_SERVER['RABBIT_RS_CONSUME_TIMEOUT_MS'] ?? '5000');

$config = [
    'brokers' => [[
        'name' => 'default',
        'hosts' => [['host' => $brokerHost, 'port' => $brokerPort]],
        'vhost' => '/',
        'credentials' => ['username' => 'rabbit_rs', 'password' => 'rabbit_rs_lab'],
        'tls' => ['enabled' => false],
        'heartbeat' => 30,
    ]],
    'workers' => [[
        'name' => 'main',
        'subscriptions' => [[
            'name' => 'default',
            'broker' => 'default',
            'queue' => $queue,
            'weight' => 1,
            'prefetch' => 16,
        ]],
        'scheduler' => [
            'strategy' => 'weighted_fair',
            'max_in_flight' => 16,
        ],
    ]],
    'topology_mode' => 'external',
];

$pool = new Goopil\RabbitRs\Pool($config);
$consumer = $pool->consumer('main');
$delivery = $consumer->next($timeoutMs);
if ($delivery === null) {
    $consumer->close();
    $pool->close();
    fwrite(STDERR, "no delivery arrived within {$timeoutMs} ms\n");
    exit(1);
}

$payload = $delivery->payload();
$messageId = $delivery->metadata()['message_id'];
$delivery->ack();
$consumer->close();
$pool->close();

header('Content-Type: application/json');
echo json_encode([
    'pid' => getmypid(),
    'payload' => $payload,
    'message_id' => $messageId,
], JSON_THROW_ON_ERROR);
