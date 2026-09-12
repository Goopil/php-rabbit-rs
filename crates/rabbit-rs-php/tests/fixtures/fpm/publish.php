<?php
declare(strict_types=1);

// FPM-resident lone-publish fixture (issue #218 certification): publishes
// exactly ONE message and performs NO follow-up operation. With
// RABBIT_RS_HOLD_MS > 0 the request keeps the pool object alive with the
// publication still buffered, so only the publish buffer's background
// age-flush timer can deliver it while the request runs.

$queue = $_SERVER['RABBIT_RS_QUEUE'] ?? '';
if ($queue === '') {
    fwrite(STDERR, "RABBIT_RS_QUEUE is required\n");
    exit(1);
}

$flushIntervalMs = (int) ($_SERVER['RABBIT_RS_FLUSH_INTERVAL_MS'] ?? '1500');
$holdMs = (int) ($_SERVER['RABBIT_RS_HOLD_MS'] ?? '0');
$markerFile = $_SERVER['RABBIT_RS_MARKER_FILE'] ?? '';
$brokerHost = $_SERVER['RABBIT_RS_BROKER_HOST'] ?? '127.0.0.1';
$brokerPort = (int) ($_SERVER['RABBIT_RS_BROKER_PORT'] ?? '5672');
$payload = $_SERVER['RABBIT_RS_PAYLOAD'] ?? 'fpm-lone-publish';

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
    'publisher' => ['flush_interval' => $flushIntervalMs],
];

$pool = new Goopil\RabbitRs\Pool($config);
$messageId = $pool->publish([
    'broker' => 'default',
    'exchange' => '', // AMQP default exchange: routes by queue name
    'routing_key' => $queue,
    'payload' => $payload,
    'message_id' => 'fpm-' . bin2hex(random_bytes(8)),
    'timeout_ms' => 30000,
]);

// Deliberately no flush(), no stats(), no size(), no close(): the lone
// publication must reach the broker through the background age-flush timer
// or the graceful-stop teardown flush, never through a follow-up operation.

if ($markerFile !== '') {
    @file_put_contents($markerFile, json_encode([
        'pid' => getmypid(),
        'message_id' => $messageId,
    ], JSON_THROW_ON_ERROR));
}

if ($holdMs > 0) {
    usleep($holdMs * 1000);
}

header('Content-Type: application/json');
echo json_encode([
    'pid' => getmypid(),
    'message_id' => $messageId,
], JSON_THROW_ON_ERROR);
