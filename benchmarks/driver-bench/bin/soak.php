#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
|--------------------------------------------------------------------------
| Round I #126 — soak: sustained pop+ack churn under scheduled kills
|--------------------------------------------------------------------------
|
| Proves the at-least-once contract over a long run: repeated fill+drain
| cycles through a dedicated toxiproxy proxy, with deterministic both-legs
| connection kills alternating post-fill (consumer re-establishes before
| the drain) and mid-drain (unacked work in flight). Loud failure on stall
| (null streak past the plausible bound) and on loss (missing > 0 at the
| end); duplicates are counted, never hidden.
|
| Requires the ext-rabbit_rs extension (runs the native Pool directly).
|
| Usage:
|   php -d extension=<artifact> bin/soak.php \
|       --minutes=10 --fill=1000 --kill-every=10 --kill-timeout-ms=50
|
| Exit 0 only when missing == 0 and the connection really re-established
| (reconnects_total >= 1). Full result as a single JSON object on stdout.
*/

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

require_once __DIR__.'/../vendor/autoload.php';

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

$args = [];
foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        $args[strtolower($m[1])] = $m[2];
    }
}

$minutes = max(1, (int) ($args['minutes'] ?? 10));
$fill = max(1, (int) ($args['fill'] ?? 1000));
$killEvery = max(0, (int) ($args['kill-every'] ?? 10));
$killTimeoutMs = max(1, (int) ($args['kill-timeout-ms'] ?? 50));
$outputPath = isset($args['output']) ? (string) $args['output'] : null;

const SOAK_QUEUE = 'bench.goopil.soak';
const SOAK_CONNECTION = 'rabbit-rs-soak';
const TOXIPROXY_API_DEFAULT = 'http://localhost:18474';
const LAB_FINGERPRINT_PROXY = 'rabbitmq-1';
const LAB_FINGERPRINT_UPSTREAM = 'rabbitmq-1:5672';
const SOAK_PROXY_PORT_MIN = 24504;
const SOAK_PROXY_PORT_MAX = 24509;

function failLoudly(string $message): never
{
    fwrite(STDERR, 'error: '.$message."\n");
    exit(1);
}

// ---------------------------------------------------------------------------
// Toxiproxy (fingerprint-checked, private proxy, deleted on shutdown)
// ---------------------------------------------------------------------------

function toxiproxyApi(): string
{
    $api = getenv('RABBIT_RS_TOXIPROXY_API');

    return $api === false || $api === '' ? TOXIPROXY_API_DEFAULT : $api;
}

/**
 * Toxiproxy REST call over the plain HTTP stream wrapper (the test suite's
 * helper uses curl; this harness keeps its own smaller dependency-free
 * implementation).
 *
 * @return array{int, string} [HTTP status, response body]
 */
function toxiproxyRequest(string $method, string $path, ?string $payload = null): array
{
    $context = stream_context_create(['http' => [
        'method' => $method,
        'timeout' => 5,
        'ignore_errors' => true,
        'header' => $payload !== null ? "Content-Type: application/json\r\n" : '',
        'content' => $payload,
    ]]);
    $body = @file_get_contents(toxiproxyApi().$path, false, $context);
    $status = isset($http_response_header[0]) && preg_match('#HTTP/\S+\s+(\d+)#', $http_response_header[0], $m)
        ? (int) $m[1]
        : 0;

    return [$status, $body === false ? '' : $body];
}

function assertLabToxiproxy(): void
{
    [$status, $body] = toxiproxyRequest('GET', '/proxies/'.LAB_FINGERPRINT_PROXY);
    $upstream = $status === 200 ? (json_decode($body, true)['upstream'] ?? '') : '';

    if ($status !== 200 || $upstream !== LAB_FINGERPRINT_UPSTREAM) {
        failLoudly(sprintf(
            'the lab Toxiproxy is not answering at %s with the "%s" fingerprint (HTTP %d, upstream "%s"); '
                .'start the lab with ./scripts/lab-up.sh',
            toxiproxyApi(),
            LAB_FINGERPRINT_PROXY,
            $status,
            $upstream,
        ));
    }
}

/**
 * @return array{name: string, port: int}
 */
function createSoakProxy(): array
{
    $name = 'soak-'.uniqid('', true);

    for ($attempt = 0; $attempt < 4; $attempt++) {
        $port = random_int(SOAK_PROXY_PORT_MIN, SOAK_PROXY_PORT_MAX);
        [$status] = toxiproxyRequest('POST', '/proxies', json_encode([
            'name' => $name,
            'listen' => '0.0.0.0:'.$port,
            'upstream' => LAB_FINGERPRINT_UPSTREAM,
            'enabled' => true,
        ]));

        if ($status === 200 || $status === 201) {
            return ['name' => $name, 'port' => $port];
        }
    }

    failLoudly("could not create soak proxy {$name} (all candidate listen ports busy)");
}

/**
 * Cuts the connection on BOTH proxy legs (broker requeues unacked work,
 * client socket dies too), waits out the resets, removes the toxics.
 */
function fireConnectionKill(string $proxy, int $timeoutMs): void
{
    foreach (['upstream', 'downstream'] as $stream) {
        [$status, $body] = toxiproxyRequest('POST', '/proxies/'.$proxy.'/toxics', json_encode([
            'name' => 'soak-kill-'.$stream,
            'type' => 'timeout',
            'stream' => $stream,
            'toxicity' => 1.0,
            'attributes' => ['timeout' => $timeoutMs],
        ]));
        if ($status !== 200) {
            failLoudly("kill toxic ({$stream}) was not applied to proxy {$proxy} (HTTP {$status}): {$body}");
        }
    }

    usleep(300000); // let the resets fire

    foreach (['upstream', 'downstream'] as $stream) {
        toxiproxyRequest('DELETE', '/proxies/'.$proxy.'/toxics/soak-kill-'.$stream);
    }
}

// ---------------------------------------------------------------------------
// Bootstrap the app (loads .env, config, registers + boots providers)
// ---------------------------------------------------------------------------

$app = require_once __DIR__.'/../bootstrap/app.php';
$consoleKernel = $app->make(Illuminate\Contracts\Console\Kernel::class);
$consoleKernel->bootstrap();

assertLabToxiproxy();
$proxy = createSoakProxy();
register_shutdown_function(static function () use ($proxy): void {
    toxiproxyRequest('DELETE', '/proxies/'.$proxy['name']);
});

// Build the pool directly (soak needs pool->stats()/flush()) from the bench
// app's rabbit-rs connection config, rerouted through the soak proxy.
$baseConfig = (array) config('queue.connections.rabbit-rs', []);
if ($baseConfig === []) {
    failLoudly('bench config queue.connections.rabbit-rs is missing');
}

$config = array_merge($baseConfig, [
    'queue' => SOAK_QUEUE,
    'hosts' => ['127.0.0.1:'.$proxy['port']],
]);
$app['config']->set('queue.connections.'.SOAK_CONNECTION, $config);

// Production parity: compile with config('rabbit-rs') as defaults (same as
// the service provider). Without it the pool's delay mode falls back to
// `auto`, whose recovery re-declares rabbit-rs.delayed (issue #97) — refused
// on the lab vhost, so every recovery dies permanently. The connector
// re-compiles internally; the factory closure below pins THIS pool, so the
// pool config must already carry the defaults.
$compiled = ConnectionCompiler::compile(SOAK_CONNECTION, $config, (array) config('rabbit-rs', []));
$pool = new Pool($compiled['native']);
$factory = new NativePoolFactory(createPool: fn (): Pool => $pool);
$queue = (new RabbitMqConnector($factory, (array) config('rabbit-rs', [])))->connect($config);
$queue->setContainer($app);
$queue->setConnectionName(SOAK_CONNECTION);

function poolReconnects(?Pool $pool): ?int
{
    if ($pool === null) {
        return null;
    }
    try {
        return $pool->stats()['reconnects_total'] ?? null;
    } catch (\Throwable) {
        return null;
    }
}

// Clean slate: purge leftovers from previous runs. Best-effort: on a fresh
// lab the queue does not exist yet (NOT_FOUND), which already is a clean
// slate — the driver declares it on the first push below.
try {
    $queue->clear(SOAK_QUEUE);
} catch (Throwable) {
    // queue missing or purge refused; leftover messages would only show up
    // as duplicates, never as a pass (missing counts soak seqs only).
}

// Ctrl-C must still run the shutdown function (proxy cleanup).
if (function_exists('pcntl_async_signals')) {
    pcntl_async_signals(true);
    pcntl_signal(SIGINT, static fn (): never => exit(130));
}

// ---------------------------------------------------------------------------
// Soak loop
// ---------------------------------------------------------------------------

$deadline = hrtime(true) + (int) ($minutes * 60 * 1e9);
$received = []; // seq => true, every payload message ever popped
$receivedTotal = 0;
$published = 0;
$cycles = 0;
$kills = 0;
$pushRetries = 0;
$popErrors = 0;
$maxAttempts = 0;
$nullStreakMax = 0;
$lastHeartbeat = microtime(true);

$nullStreakCap = max(200_000, $fill * 200); // ~50 s of pure emptiness mid-drain = stall
$grace = (int) (120 * 1e9); // drain may outlive the soak deadline briefly

fwrite(STDERR, sprintf(
    "soak: %d min, fill %d/cycle, kill every %d cycle(s) (%d ms both-legs timeout), proxy :%d\n",
    $minutes,
    $fill,
    $killEvery,
    $killTimeoutMs,
    $proxy['port'],
));

while (hrtime(true) < $deadline) {
    $killPostFill = $killEvery > 0 && $cycles % (2 * $killEvery) === 0;
    $killMidDrain = $killEvery > 0 && $cycles % (2 * $killEvery) === $killEvery;

    // Fill: every message must be CONFIRMED on the broker before the drain.
    for ($i = 0; $i < $fill; $i++) {
        $seq = ++$published;
        $tries = 0;
        while (true) {
            try {
                $queue->push('stdClass', ['msg' => (string) $seq], SOAK_QUEUE);
                break;
            } catch (QueueException | ConnectionException $e) {
                $lastPushError = $e->getMessage();
                $pushRetries++;
                if (++$tries > 300) {
                    failLoudly("push for seq {$seq} kept failing for 30 s (fill aborted); last error: {$lastPushError}");
                }
                usleep(100000);
            }
        }
    }

    // Flush until success: a publication held in the process buffer is not
    // on the wire, and the drain below would never see it.
    $tries = 0;
    while (true) {
        try {
            $pool->flush();
            break;
        } catch (\Throwable) {
            $pushRetries++;
            if (++$tries > 300) {
                failLoudly('pool flush kept failing for 30 s (fill not confirmed, aborting)');
            }
            usleep(100000);
        }
    }

    if ($killPostFill) {
        fireConnectionKill($proxy['name'], $killTimeoutMs);
        $kills++;
    }

    // Drain: pop + ack until this cycle's fill is consumed. A kill may land
    // mid-drain with unacked work in flight; the pool must recover on its own.
    $receivedThisCycle = 0;
    $consecutiveNulls = 0;
    $midKillAt = $killMidDrain ? intdiv($fill, 2) : -1;
    $midKillDone = false;

    while ($receivedThisCycle < $fill) {
        if (hrtime(true) > $deadline + $grace) {
            failLoudly(sprintf(
                'drain did not converge within the grace window (cycle %d, received %d/%d this cycle, %d/%d distinct/total overall)',
                $cycles,
                $receivedThisCycle,
                $fill,
                count($received),
                $published,
            ));
        }

        if ($midKillAt >= 0 && ! $midKillDone && $receivedThisCycle >= $midKillAt) {
            $midKillDone = true;
            fireConnectionKill($proxy['name'], $killTimeoutMs);
            $kills++;
        }

        try {
            $job = $queue->pop(SOAK_QUEUE);
        } catch (QueueException | ConnectionException) {
            $popErrors++;
            usleep(100000);
            continue;
        }

        if ($job === null) {
            $consecutiveNulls++;
            $nullStreakMax = max($nullStreakMax, $consecutiveNulls);
            if ($consecutiveNulls > $nullStreakCap) {
                failLoudly(sprintf(
                    'consumer stopped receiving while work is owed (null streak %d, cycle %d, received %d/%d this cycle, %d/%d distinct/total overall) — possible stall',
                    $consecutiveNulls,
                    $cycles,
                    $receivedThisCycle,
                    $fill,
                    count($received),
                    $published,
                ));
            }
            usleep(250);
            continue;
        }

        $consecutiveNulls = 0;
        $body = json_decode($job->getRawBody(), true);
        $seq = (int) ($body['data']['msg'] ?? 0);
        $received[$seq] = true;
        $receivedTotal++;
        $receivedThisCycle++;
        $maxAttempts = max($maxAttempts, $job->attempts());

        try {
            $job->delete();
        } catch (QueueException | ConnectionException) {
            // ack lost (e.g. stale after a kill): the broker requeues and the
            // message comes back as a redelivery — counted as a duplicate.
            $popErrors++;
        }
    }

    $cycles++;

    if (microtime(true) - $lastHeartbeat > 30) {
        $lastHeartbeat = microtime(true);
        fwrite(STDERR, sprintf(
            "soak: t=%.0fs cycles=%d published=%d received=%d (distinct %d) kills=%d reconnects=%s\n",
            (hrtime(true) - $deadline + $minutes * 60 * 1e9) / 1e9,
            $cycles,
            $published,
            $receivedTotal,
            count($received),
            $kills,
            var_export(poolReconnects($pool), true),
        ));
    }
}

// ---------------------------------------------------------------------------
// Final drain: everything published must show up (at-least-once), then the
// queue must stay quiet.
// ---------------------------------------------------------------------------

$lastReceipt = microtime(true);
while ((microtime(true) - $lastReceipt) < 3.0) {
    try {
        $job = $queue->pop(SOAK_QUEUE);
    } catch (QueueException | ConnectionException) {
        $popErrors++;
        usleep(100000);
        continue;
    }
    if ($job === null) {
        usleep(50000);
        continue;
    }
    $lastReceipt = microtime(true);
    $body = json_decode($job->getRawBody(), true);
    $seq = (int) ($body['data']['msg'] ?? 0);
    $received[$seq] = true;
    $receivedTotal++;
    $maxAttempts = max($maxAttempts, $job->attempts());
    try {
        $job->delete();
    } catch (QueueException | ConnectionException) {
        $popErrors++;
    }
}

$distinct = count($received);
$missing = $published - $distinct;
$duplicates = $receivedTotal - $distinct;
$reconnects = poolReconnects($pool);

try {
    $pool->close();
} catch (Throwable) {
    // best-effort cleanup
}

$ok = $missing === 0 && $reconnects !== null && $reconnects >= 1;

$result = [
    'benchmark' => 'soak',
    'ok' => $ok,
    'minutes' => $minutes,
    'fill' => $fill,
    'kill_every' => $killEvery,
    'kill_timeout_ms' => $killTimeoutMs,
    'queue' => SOAK_QUEUE,
    'duration_s' => round($minutes * 60, 0),
    'cycles' => $cycles,
    'kills' => $kills,
    'reconnects_total' => $reconnects,
    'published' => $published,
    'received_total' => $receivedTotal,
    'distinct_received' => $distinct,
    'missing' => $missing,
    'duplicates' => $duplicates,
    'max_attempts' => $maxAttempts,
    'null_streak_max' => $nullStreakMax,
    'pop_errors' => $popErrors,
    'push_retries' => $pushRetries,
];

$json = json_encode($result, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
if ($outputPath !== null) {
    file_put_contents($outputPath, $json.PHP_EOL);
}
fwrite(STDOUT, $json.PHP_EOL);

exit($ok ? 0 : 1);
