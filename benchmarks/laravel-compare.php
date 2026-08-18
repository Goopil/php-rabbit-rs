<?php

declare(strict_types=1);

if (! extension_loaded('rabbit_rs')) {
    fwrite(STDERR, "Error: ext-rabbit_rs is not loaded.\n");
    exit(1);
}

require __DIR__ . '/vendor/autoload.php';

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

const BASE_DIR = __DIR__;
const VHOST = '/orders-eu';
const BROKER_HOST = '127.0.0.1';
const BROKER_PORT = 5672;
const ADMIN_USER = 'admin';
const ADMIN_PASS = 'admin_lab';
const RABBIT_USER = 'rabbit_rs';
const RABBIT_PASS = 'rabbit_rs_lab';

$opts = getopt('', ['count:', 'payload:', 'driver:', 'help']);
if (isset($opts['help'])) {
    fwrite(STDOUT, "Usage: php laravel-compare.php [--count N] [--driver rabbit-rs|php-amqplib|vyuldashev|all]\n");
    fwrite(STDOUT, "  --count    Number of messages per run (default: 1000)\n");
    fwrite(STDOUT, "  --driver   Driver to test (default: all)\n");
    fwrite(STDOUT, "  --payload  Payload size in bytes (default: 256)\n");
    exit(0);
}

$count = (int) ($opts['count'] ?? 1000);
$payloadSize = (int) ($opts['payload'] ?? 256);
$driverFilter = $opts['driver'] ?? 'all';

$drivers = match ($driverFilter) {
    'rabbit-rs' => ['rabbit-rs'],
    'php-amqplib' => ['php-amqplib'],
    'vyuldashev' => ['vyuldashev'],
    default => ['rabbit-rs', 'php-amqplib', 'vyuldashev'],
};

$payload = str_repeat('x', $payloadSize);

fwrite(STDOUT, "Laravel Comparative Benchmark\n");
fwrite(STDOUT, str_repeat('=', 50) . "\n\n");
fwrite(STDOUT, "Environment:\n");
fwrite(STDOUT, "  PHP: " . PHP_VERSION . "\n");
fwrite(STDOUT, "  SAPI: " . PHP_SAPI . "\n");
fwrite(STDOUT, "  Count: {$count} msgs, payload {$payloadSize}B\n\n");

$results = [];

foreach ($drivers as $driverName) {
    fwrite(STDOUT, "Driver: {$driverName}\n");

    $queueName = "bench-laravel-{$driverName}-" . uniqid('', true);
    declareQueue($queueName);

    try {
        $result = match ($driverName) {
            'rabbit-rs' => benchRabbitRs($queueName, $count, $payload),
            'php-amqplib' => class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)
                ? benchPhpAmqplib($queueName, $count, $payload)
                : skipDriver('php-amqplib (not installed)'),
            'vyuldashev' => class_exists(\VladimirYuldashev\LaravelQueueRabbitMQ\Queue\Connectors\RabbitMQConnector::class)
                ? benchVyuldashev($queueName, $count, $payload)
                : skipDriver('vyuldashev (not installed)'),
        };
        $results[] = $result;
        fwrite(STDOUT, sprintf("  Publish:  %.0f msgs/s\n", $result['publish_throughput']));
        fwrite(STDOUT, sprintf("  Consume:  %.0f msgs/s\n", $result['consume_throughput']));
        fwrite(STDOUT, sprintf("  Losses:   %d\n\n", $result['losses']));
    } catch (\Throwable $e) {
        fwrite(STDOUT, "  FAIL: " . $e->getMessage() . "\n\n");
        $results[] = [
            'driver' => $driverName,
            'publish_throughput' => 0,
            'consume_throughput' => 0,
            'losses' => -1,
            'error' => $e->getMessage(),
        ];
    } finally {
        deleteQueue($queueName);
    }
}

fwrite(STDOUT, "\n" . str_repeat('=', 50) . "\n");
fwrite(STDOUT, "Comparison ({$count} msgs, {$payloadSize}B):\n\n");
fwrite(STDOUT, sprintf("%-15s %15s %15s %10s\n", 'Driver', 'Publish (msgs/s)', 'Consume (msgs/s)', 'Losses'));
fwrite(STDOUT, str_repeat('-', 55) . "\n");
foreach ($results as $r) {
    if (isset($r['error'])) {
        fwrite(STDOUT, sprintf("%-15s %15s\n", $r['driver'], 'ERROR: ' . $r['error']));
    } else {
        fwrite(STDOUT, sprintf("%-15s %15.0f %15.0f %10d\n", $r['driver'], $r['publish_throughput'], $r['consume_throughput'], $r['losses']));
    }
}

$jsonPath = BASE_DIR . '/results/laravel-compare-' . date('Ymd-His') . '.json';
if (!is_dir(dirname($jsonPath))) {
    mkdir(dirname($jsonPath), 0755, true);
}
file_put_contents($jsonPath, json_encode(['results' => $results], JSON_PRETTY_PRINT));
fwrite(STDOUT, "\nResults saved to: {$jsonPath}\n");

function benchRabbitRs(string $queueName, int $count, string $payload): array
{
    $config = liveConfig($queueName);
    $normalized = ConfigNormalizer::normalize($config);
    $pool = new Pool($normalized['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $pool);
    $connector = new RabbitMqConnector($factory, $normalized);
    $queue = $connector->connect(['queue' => $queueName, 'block_for' => 3]);
    $container = new \Illuminate\Container\Container();
    $container->instance('config', new \Illuminate\Config\Repository());
    $queue->setContainer($container);
    $queue->setConnectionName('rabbit-rs-bench');
    $queue->clear($queueName);

    $publishStart = hrtime(true);
    for ($i = 0; $i < $count; $i++) {
        $queue->push('stdClass', ['index' => $i, 'data' => $payload], $queueName);
    }
    $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000_000;

    $consumeStart = hrtime(true);
    $received = 0;
    $nulls = 0;
    while ($received < $count) {
        $job = $queue->pop($queueName);
        if ($job === null) {
            if (++$nulls >= 5) break;
            continue;
        }
        $nulls = 0;
        $job->delete();
        $received++;
    }
    $consumeElapsed = (hrtime(true) - $consumeStart) / 1_000_000_000;

    $queue->clear($queueName);
    $pool->close();

    return [
        'driver' => 'rabbit-rs',
        'publish_throughput' => $publishElapsed > 0 ? $count / $publishElapsed : 0,
        'consume_throughput' => $consumeElapsed > 0 ? $received / $consumeElapsed : 0,
        'losses' => $count - $received,
    ];
}

function benchPhpAmqplib(string $queueName, int $count, string $payload): array
{
    $conn = new \PhpAmqpLib\Connection\AMQPStreamConnection(
        BROKER_HOST, BROKER_PORT, RABBIT_USER, RABBIT_PASS, VHOST
    );
    $pubChannel = $conn->channel();
    $consChannel = $conn->channel();
    $pubChannel->queue_declare($queueName, false, true, false, false);
    $consChannel->basic_qos(0, 16, false);
    $consChannel->queue_declare($queueName, false, true, false, false);

    $publishStart = hrtime(true);
    for ($i = 0; $i < $count; $i++) {
        $body = json_encode(['job' => 'stdClass', 'data' => ['index' => $i, 'data' => $payload], 'uuid' => uniqid(), 'attempts' => 0]);
        $msg = new \PhpAmqpLib\Message\AMQPMessage($body, [
            'delivery_mode' => \PhpAmqpLib\Message\AMQPMessage::DELIVERY_MODE_PERSISTENT,
            'message_id' => uniqid('', true),
        ]);
        $pubChannel->basic_publish($msg, '', $queueName);
    }
    $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000_000;

    $consumeStart = hrtime(true);
    $received = 0;
    $callback = function ($msg) use (&$received): void {
        $msg->ack();
        $received++;
    };
    $consChannel->basic_consume($queueName, '', false, false, false, false, $callback);
    $nulls = 0;
    while ($received < $count) {
        try {
            $consChannel->wait(null, false, 1);
            $nulls = 0;
        } catch (\PhpAmqpLib\Exception\AMQPTimeoutException) {
            if (++$nulls >= 5) break;
        }
    }
    $consumeElapsed = (hrtime(true) - $consumeStart) / 1_000_000_000;

    $pubChannel->close();
    $consChannel->close();
    $conn->close();

    return [
        'driver' => 'php-amqplib',
        'publish_throughput' => $publishElapsed > 0 ? $count / $publishElapsed : 0,
        'consume_throughput' => $consumeElapsed > 0 ? $received / $consumeElapsed : 0,
        'losses' => $count - $received,
    ];
}

function benchVyuldashev(string $queueName, int $count, string $payload): array
{
    $container = new \Illuminate\Container\Container();
    $container->instance('config', new \Illuminate\Config\Repository());
    $events = new \Illuminate\Events\Dispatcher($container);
    $queueManager = new \Illuminate\Queue\QueueManager($events);
    $queueManager->addConnector('rabbitmq', fn () => new \VladimirYuldashev\LaravelQueueRabbitMQ\Queue\Connectors\RabbitMQConnector($events));

    $config = [
        'driver' => 'rabbitmq',
        'queue' => $queueName,
        'connection' => 'default',
        'hosts' => [[
            'host' => BROKER_HOST,
            'port' => BROKER_PORT,
            'vhost' => VHOST,
            'user' => RABBIT_USER,
            'password' => RABBIT_PASS,
        ]],
        'options' => [
            'exchange' => '',
            'exchange_type' => 'direct',
            'exchange_routing_key' => '',
            'with_queue' => true,
            'queue_passive' => false,
            'queue_durable' => true,
            'queue_exclusive' => false,
            'queue_auto_delete' => false,
            'queue_arguments' => ['x-queue-type' => 'quorum'],
        ],
    ];
    $container['config']->set('queue.connections.rabbitmq', $config);
    $container['config']->set('queue.default', 'rabbitmq');

    $queue = $queueManager->connection('rabbitmq');
    $queue->setContainer($container);

    $queue->purge($queueName);

    $publishStart = hrtime(true);
    for ($i = 0; $i < $count; $i++) {
        $queue->pushRaw(
            json_encode(['job' => 'stdClass', 'data' => ['index' => $i, 'data' => $payload], 'uuid' => uniqid(), 'attempts' => 0]),
            $queueName
        );
    }
    $publishElapsed = (hrtime(true) - $publishStart) / 1_000_000_000;

    $consumeStart = hrtime(true);
    $received = 0;
    $nulls = 0;
    while ($received < $count) {
        $job = $queue->pop($queueName);
        if ($job === null) {
            if (++$nulls >= 5) break;
            continue;
        }
        $job->delete();
        $received++;
    }
    $consumeElapsed = (hrtime(true) - $consumeStart) / 1_000_000_000;

    $queue->purge($queueName);

    return [
        'driver' => 'vyuldashev',
        'publish_throughput' => $publishElapsed > 0 ? $count / $publishElapsed : 0,
        'consume_throughput' => $consumeElapsed > 0 ? $received / $consumeElapsed : 0,
        'losses' => $count - $received,
    ];
}

function skipDriver(string $reason): array
{
    fwrite(STDOUT, "  SKIP: {$reason}\n\n");
    return ['driver' => $reason, 'publish_throughput' => 0, 'consume_throughput' => 0, 'losses' => -1, 'error' => $reason];
}

function liveConfig(string $queueName): array
{
    return [
        'topology_mode' => 'declare',
        'brokers' => [
            'default' => [
                'hosts' => ['127.0.0.1:5672'],
                'vhost' => VHOST,
                'credentials' => [
                    'username' => RABBIT_USER,
                    'password' => RABBIT_PASS,
                ],
                'tls' => ['enabled' => false, 'server_name' => null],
                'heartbeat' => 30,
            ],
        ],
        'routes' => [
            'default' => [
                'broker' => 'default',
                'exchange' => '',
                'routing_key' => '{queue}',
            ],
        ],
        'workers' => [
            'default' => [
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
                'subscriptions' => [
                    'default' => [
                        'enabled' => true,
                        'broker' => 'default',
                        'queue' => $queueName,
                        'weight' => 1,
                        'priority_class' => 0,
                        'prefetch' => ['mode' => 'fixed', 'value' => 16],
                        'starvation_after' => 30,
                    ],
                ],
            ],
        ],
        'publisher' => [
            'confirms' => true,
            'mandatory' => true,
        ],
        'topology' => [
            'queue' => [
                'type' => 'quorum',
                'durable' => true,
                'delivery_limit' => 20,
            ],
            'dead_letter' => null,
        ],
    ];
}

function declareQueue(string $queueName): void
{
    $url = 'http://localhost:15672/api/queues/' . urlencode(VHOST) . '/' . urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
    curl_setopt($ch, CURLOPT_POSTFIELDS, json_encode(['durable' => true, 'arguments' => ['x-queue-type' => 'quorum']]));
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, ADMIN_USER . ':' . ADMIN_PASS);
    curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    curl_exec($ch);
    curl_close($ch);
}

function deleteQueue(string $queueName): void
{
    $url = 'http://localhost:15672/api/queues/' . urlencode(VHOST) . '/' . urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, ADMIN_USER . ':' . ADMIN_PASS);
    curl_exec($ch);
    curl_close($ch);
}
