<?php

declare(strict_types=1);

/*
 * Round L profile tool — fills the bench queue and exits.
 *
 * Separated from micro-consume.php so a consumer-side profile records only
 * the drain loop (the consuming process bootstraps against an already-full
 * queue; the fill process is never profiled).
 *
 * Usage: php -d extension=<dylib> fill.php --count=30000
 */

$args = [];
foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        $args[strtolower($m[1])] = $m[2];
    }
}

$count = max(1, (int) ($args['count'] ?? 30_000));
$poolDir = dirname(__DIR__, 3);

require $poolDir.'/driver-bench/vendor/autoload.php';
$app = require $poolDir.'/driver-bench/bootstrap/app.php';
$app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();

$connectionConfig = (array) config('queue.connections.rabbit-rs', []);
$queueName = (string) ($connectionConfig['queue'] ?? 'bench.goopil.driver-bench');
$compiled = Goopil\RabbitRs\Laravel\Config\ConnectionCompiler::compile(
    'rabbit-rs',
    $connectionConfig,
    (array) config('rabbit-rs', []),
);
$factory = $app->make(Goopil\RabbitRs\Laravel\Support\NativePoolFactory::class);
$pool = $factory->make($compiled['native']);
$broker = (string) $compiled['native']['brokers'][0]['name'];
$routingKey = str_replace('{queue}', $queueName, (string) ($compiled['native']['routes']['default']['routing_key'] ?? $queueName));

$pool->clear($broker, $queueName);

$payload = str_repeat('x', 1024 - 60);
$fillStart = hrtime(true);
$messages = [];
for ($i = 0; $i < $count; $i++) {
    $messages[] = [
        'broker' => $broker,
        'exchange' => '',
        'routing_key' => $routingKey,
        'payload' => $payload,
        'message_id' => 'round-l-'.$i.'-'.bin2hex(random_bytes(4)),
        'content_type' => 'application/json',
    ];
    if (count($messages) === 256) {
        $pool->publishBatch($messages);
        $messages = [];
    }
}
if ($messages !== []) {
    $pool->publishBatch($messages);
}
$pool->flush();

$deadline = hrtime(true) + 60_000_000_000;
while (hrtime(true) < $deadline) {
    if ($pool->size($broker, $queueName) >= $count) {
        break;
    }
    usleep(20_000);
}

printf(
    "filled %d, ingestion confirmed, %s\n",
    $count,
    json_encode(['fill_s' => round((hrtime(true) - $fillStart) / 1e9, 2), 'queue_size' => $pool->size($broker, $queueName)]),
);
$pool->close();
