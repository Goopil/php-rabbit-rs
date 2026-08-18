<?php

declare(strict_types=1);

if (! extension_loaded('rabbit_rs')) {
    fwrite(STDERR, "Error: ext-rabbit_rs is not loaded. Build and install the extension first.\n");
    exit(1);
}

require __DIR__ . '/../packages/laravel-queue/vendor/autoload.php';

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

const SMOKE_COUNT = 100;
const QUEUE_PREFIX = 'bench-laravel-smoke';

$queueName = QUEUE_PREFIX . '-' . uniqid('', true);

fwrite(STDOUT, "Laravel Queue Smoke Benchmark\n");
fwrite(STDOUT, str_repeat('=', 40) . "\n\n");

declareQueue($queueName);

$config = liveConfig($queueName);
$normalized = ConfigNormalizer::normalize($config);

$pool = new Pool($normalized['native']);
$factory = new NativePoolFactory(createPool: fn (): Pool => $pool);
$connector = new RabbitMqConnector($factory, $normalized);

$queue = $connector->connect([
    'queue' => $queueName,
    'block_for' => 3,
]);

$container = new \Illuminate\Container\Container();
$container->instance('config', new \Illuminate\Config\Repository());
$queue->setContainer($container);
$queue->setConnectionName('rabbit-rs-bench');

$queue->clear($queueName);

fwrite(STDOUT, "Publishing " . SMOKE_COUNT . " messages...\n");
$publishStart = hrtime(true);

for ($i = 0; $i < SMOKE_COUNT; $i++) {
    $queue->push('stdClass', ['index' => $i], $queueName);
}

$publishElapsed = (hrtime(true) - $publishStart) / 1_000_000_000;
$publishThroughput = $publishElapsed > 0 ? SMOKE_COUNT / $publishElapsed : 0.0;
fwrite(STDOUT, sprintf("  Done in %.3fs (%.0f msgs/s)\n\n", $publishElapsed, $publishThroughput));

fwrite(STDOUT, "Consuming " . SMOKE_COUNT . " messages...\n");
$consumeStart = hrtime(true);
$received = 0;
$consecutiveNulls = 0;

while ($received < SMOKE_COUNT) {
    $job = $queue->pop($queueName);
    if ($job === null) {
        $consecutiveNulls++;
        if ($consecutiveNulls >= 5) {
            break;
        }
        continue;
    }
    $consecutiveNulls = 0;
    $job->delete();
    $received++;
}

$consumeElapsed = (hrtime(true) - $consumeStart) / 1_000_000_000;
$consumeThroughput = $consumeElapsed > 0 ? $received / $consumeElapsed : 0.0;
$losses = SMOKE_COUNT - $received;

fwrite(STDOUT, sprintf("  Received %d in %.3fs (%.0f msgs/s)\n\n", $received, $consumeElapsed, $consumeThroughput));

fwrite(STDOUT, "Results:\n");
fwrite(STDOUT, sprintf("  Publish:  %.0f msgs/s\n", $publishThroughput));
fwrite(STDOUT, sprintf("  Consume:  %.0f msgs/s\n", $consumeThroughput));
fwrite(STDOUT, sprintf("  Received: %d / %d\n", $received, SMOKE_COUNT));
fwrite(STDOUT, sprintf("  Losses:   %d\n", $losses));
fwrite(STDOUT, "\n");

$pass = ($losses === 0) && ($received === SMOKE_COUNT);
fwrite(STDOUT, "Result: " . ($pass ? 'PASS' : 'FAIL') . "\n");

$queue->clear($queueName);
$pool->close();
deleteQueue($queueName);

exit($pass ? 0 : 1);

function liveConfig(string $queueName): array
{
    return [
        'topology_mode' => 'declare',
        'brokers' => [
            'default' => [
                'hosts' => ['127.0.0.1:5672'],
                'vhost' => '/orders-eu',
                'credentials' => [
                    'username' => 'rabbit_rs',
                    'password' => 'rabbit_rs_lab',
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
    $url = 'http://localhost:15672/api/queues/%2Forders-eu/' . urlencode($queueName);
    $payload = json_encode([
        'durable' => true,
        'arguments' => ['x-queue-type' => 'quorum', 'x-delivery-limit' => 20],
    ]);

    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
    curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    curl_exec($ch);
    curl_close($ch);
}

function deleteQueue(string $queueName): void
{
    $url = 'http://localhost:15672/api/queues/%2Forders-eu/' . urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_exec($ch);
    curl_close($ch);
}
