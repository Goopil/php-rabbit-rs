<?php

declare(strict_types=1);

/*
 * AMQP functional smoke for the rabbit_rs extension (issue #227).
 *
 * Proves the functional bar against a real broker: extension load, publish +
 * confirms, consume + ack, and one Toxiproxy outage/recovery scenario
 * (publish during an outage must buffer and confirm after the network heals,
 * per the at-least-once contract).
 *
 * Usage:
 *   php run.php --dsn=127.0.0.1:5672 --toxiproxy=http://127.0.0.1:18474 \
 *       [--skip-recovery]
 *
 * Defaults match the lab (./scripts/lab-up.sh with-plugin): broker on
 * 127.0.0.1:5672, Toxiproxy API on 127.0.0.1:18474, vhost "/", user
 * rabbit_rs / rabbit_rs_lab. RABBIT_RS_SMOKE_VHOST / _USER / _PASSWORD
 * override the broker identity.
 *
 * Requires ext-rabbit_rs loaded (load it with -d extension=<artifact>).
 * Exit 0 only when every phase passes; any loss or error exits non-zero
 * with a message on stderr. The Toxiproxy proxy this script creates is
 * always deleted (explicitly and via a shutdown function), so repeated
 * runs are idempotent.
 */

use Goopil\RabbitRs\Pool;

const BROKER_USER_DEFAULT = 'rabbit_rs';
const BROKER_PASSWORD_DEFAULT = 'rabbit_rs_lab';
const VHOST_DEFAULT = '/';
const EXCHANGE = '';
const PUBLISH_TIMEOUT_MS = 10000;
const RECOVERY_PUBLISH_TIMEOUT_MS = 60000;
const CONSUME_TIMEOUT_MS = 500;
const CONSUME_DEADLINE_SECONDS = 60;
const RECOVERY_DEADLINE_SECONDS = 180;
const HEARTBEAT_SECONDS = 30;
const TOXIPROXY_TIMEOUT = 5;
const LAB_FINGERPRINT_PROXY = 'rabbitmq-1';
const LAB_FINGERPRINT_UPSTREAM = 'rabbitmq-1:5672';
const PROXY_PORT_MIN = 24504;
const PROXY_PORT_MAX = 24509;
const PROXY_CREATE_ATTEMPTS = 6;

/**
 * Fails loudly: message on stderr, non-zero exit. The shutdown function
 * still runs, so the Toxiproxy proxy never leaks.
 */
function fail(string $message): never
{
    fwrite(STDERR, '[amqp-smoke] FAIL: '.$message."\n");
    exit(1);
}

function pass(string $message): void
{
    echo '[amqp-smoke] '.$message."\n";
}

/**
 * @return array<string, string>
 */
function parseArgs(array $argv): array
{
    $args = [];
    foreach (array_slice($argv, 1) as $arg) {
        if (preg_match('/^--([a-z0-9-]+)=(.*)$/', (string) $arg, $m) === 1) {
            $args[$m[1]] = $m[2];

            continue;
        }
        if ($arg === '--skip-recovery') {
            $args['skip-recovery'] = '1';

            continue;
        }
        fail("unknown argument '{$arg}'; expected --dsn=<host:port> --toxiproxy=<url> [--skip-recovery]");
    }

    return $args;
}

/**
 * Toxiproxy REST call over the plain HTTP stream wrapper (no curl
 * dependency: the Alpine docker cells ship no curl PHP extension).
 *
 * @return array{int, string} [HTTP status, response body]
 */
function toxiproxyRequest(string $api, string $method, string $path, ?string $payload = null): array
{
    $context = stream_context_create(['http' => [
        'method' => $method,
        'timeout' => TOXIPROXY_TIMEOUT,
        'ignore_errors' => true,
        'header' => $payload !== null ? "Content-Type: application/json\r\n" : '',
        'content' => $payload,
    ]]);
    $body = @file_get_contents($api.$path, false, $context);
    $headers = http_get_last_response_headers() ?? [];
    $status = isset($headers[0]) && preg_match('#HTTP/\S+\s+(\d+)#', (string) $headers[0], $m) === 1
        ? (int) $m[1]
        : 0;

    return [$status, $body === false ? '' : $body];
}

/**
 * Fails unless the Toxiproxy answering on the API port is the lab's own
 * instance (fingerprint proxy rabbitmq-1 upstream rabbitmq-1:5672) — a
 * foreign instance must never receive outage injections meant for the lab.
 */
function assertLabToxiproxy(string $api): void
{
    [$status, $body] = toxiproxyRequest($api, 'GET', '/proxies/'.LAB_FINGERPRINT_PROXY);
    $upstream = $status === 200 ? (json_decode($body, true)['upstream'] ?? '') : '';

    if ($status !== 200 || $upstream !== LAB_FINGERPRINT_UPSTREAM) {
        fail(sprintf(
            'the lab Toxiproxy is not answering at %s with the "%s" fingerprint '
                .'(HTTP %d, upstream "%s"); start the lab with ./scripts/lab-up.sh with-plugin',
            $api,
            LAB_FINGERPRINT_PROXY,
            $status,
            $upstream === '' ? 'none' : $upstream,
        ));
    }
}

/**
 * Creates a private proxy upstream to the lab's rabbitmq-1 node. Always
 * deleted at shutdown (success, failure, and exit alike).
 *
 * @return array{name: string, listen: string, upstream: string, port: int}
 */
function createRecoveryProxy(string $api): array
{
    $name = 'amqp-smoke-'.uniqid('', true);
    $lastStatus = 0;

    for ($attempt = 0; $attempt < PROXY_CREATE_ATTEMPTS; $attempt++) {
        $port = random_int(PROXY_PORT_MIN, PROXY_PORT_MAX);
        $listen = '0.0.0.0:'.$port;
        $upstream = LAB_FINGERPRINT_UPSTREAM;
        [$lastStatus] = toxiproxyRequest($api, 'POST', '/proxies', json_encode([
            'name' => $name,
            'listen' => $listen,
            'upstream' => $upstream,
            'enabled' => true,
        ]));

        if ($lastStatus === 200 || $lastStatus === 201) {
            register_shutdown_function(static function () use ($api, $name): void {
                toxiproxyRequest($api, 'DELETE', '/proxies/'.$name);
            });

            return ['name' => $name, 'listen' => $listen, 'upstream' => $upstream, 'port' => $port];
        }
    }

    fail("could not create recovery proxy {$name} on {$api} (listen ports busy, last HTTP {$lastStatus})");
}

/**
 * Enables or disables the proxy. Toxiproxy has no dedicated disable
 * endpoint: a proxy update (POST /proxies/{name} with the full definition)
 * with `enabled: false` stops the listener and closes active connections;
 * re-enabling accepts new ones again.
 */
function proxyToggle(string $api, string $name, string $listen, string $upstream, bool $enabled): void
{
    [$status, $body] = toxiproxyRequest($api, 'POST', '/proxies/'.$name, json_encode([
        'name' => $name,
        'listen' => $listen,
        'upstream' => $upstream,
        'enabled' => $enabled,
    ]));

    if ($status !== 200) {
        $action = $enabled ? 'enable' : 'disable';
        fail("toxiproxy {$action} on proxy {$name} failed (HTTP {$status}): {$body}");
    }
}

/**
 * Builds a native pool config for one broker host, subscribing a single
 * worker to the given queue (declare mode: the pool provisions the queue).
 *
 * Delay mode is ttl: the default auto strategy declares the
 * `rabbit-rs.delayed` exchange, which the lab's rabbit_rs user may not
 * configure on the smoke's default vhost ("/" allows bench.* queue names
 * only). The smoke never uses delayed delivery, so the TTL strategy — which
 * declares no extra topology — is the right fit for a least-privilege lab.
 *
 * @return array<string, mixed>
 */
function poolConfig(string $host, int $port, string $vhost, string $user, string $password, string $queue): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => $host, 'port' => $port]],
            'vhost' => $vhost,
            'credentials' => ['username' => $user, 'password' => $password],
            'tls' => ['enabled' => false],
            'heartbeat' => HEARTBEAT_SECONDS,
        ]],
        'workers' => [[
            'name' => 'smoke',
            'subscriptions' => [[
                'name' => 'default',
                'broker' => 'default',
                'queue' => $queue,
                'weight' => 1,
                'prefetch' => 16,
            ]],
            'scheduler' => ['strategy' => 'weighted_fair'],
        ]],
        'topology_mode' => 'declare',
        'delay' => ['mode' => 'ttl'],
    ];
}

/**
 * @return array{broker: string, exchange: string, routing_key: string, payload: string, message_id: string, timeout_ms: int}
 */
function message(string $queue, string $runId, string $id, int $timeoutMs): array
{
    return [
        'broker' => 'default',
        'exchange' => EXCHANGE,
        'routing_key' => $queue,
        'payload' => json_encode(['smoke' => $runId, 'id' => $id]),
        'message_id' => $id,
        'timeout_ms' => $timeoutMs,
    ];
}

/**
 * Consumes deliveries until every expected message id has arrived, acking
 * everything received, verifying each payload carries the run tag, and
 * asserting the queue drains to 0 while the consumer is still open.
 * Returns the duplicate count (permitted, counted — never hidden).
 *
 * @param array<string, true> $expectedIds
 */
function consumeAll(Pool $pool, string $queue, array $expectedIds, string $label, string $runId): int
{
    $consumer = $pool->consumer('smoke');
    $received = [];
    $duplicates = 0;
    $deadline = microtime(true) + CONSUME_DEADLINE_SECONDS;

    while (count(array_diff_key($expectedIds, $received)) > 0) {
        if (microtime(true) > $deadline) {
            fail('consume deadline exhausted; still missing: '
                .implode(', ', array_keys(array_diff_key($expectedIds, $received))));
        }

        $delivery = $consumer->next(CONSUME_TIMEOUT_MS);
        if ($delivery === null) {
            continue;
        }

        $id = $delivery->metadata()['message_id'] ?? '';
        if (! isset($expectedIds[$id])) {
            fail('received unexpected message id "'.$id.'" (payload: '.$delivery->payload().')');
        }
        $payload = json_decode($delivery->payload(), true);
        if (($payload['smoke'] ?? '') !== $runId) {
            fail('received a payload from another run: '.$delivery->payload());
        }
        if (isset($received[$id])) {
            $duplicates++;
        }
        $received[$id] = true;
        $delivery->ack();
    }

    $errors = $consumer->drainErrors();
    if ($errors !== []) {
        fail('settlement errors after consume: '.json_encode($errors));
    }

    assertDrained($pool, $queue);

    $consumer->close();

    pass("{$label}: consumed + acked ".count($received).' message(s), duplicates = '.$duplicates);

    return $duplicates;
}

/**
 * Asserts the queue drains to depth 0. Acknowledgements are fire-and-forget,
 * so the depth read races the final ack: poll briefly instead of asserting
 * the first sample.
 *
 * Must be called while the consumer is still open: closing the handle before
 * the acks settle cancels the channel and RabbitMQ requeues the unacked
 * deliveries, which would defeat the depth assertion.
 */
function assertDrained(Pool $pool, string $queue): void
{
    $deadline = microtime(true) + 10;
    do {
        $depth = $pool->size('default', $queue);
        if ($depth === 0) {
            return;
        }
        usleep(200000);
    } while (microtime(true) <= $deadline);

    fail("queue {$queue} depth is {$depth} after consuming every message; expected 0");
}

/**
 * Phase A: publish 5 through the direct broker DSN, assert all confirmed
 * (no errors, confirms counted), consume + ack all 5 exactly once, queue
 * drained to depth 0.
 */
function phaseDirect(Pool $pool, string $queue, string $runId, int $count): void
{
    $ids = [];
    $messages = [];
    for ($i = 1; $i <= $count; $i++) {
        $id = "smoke-{$runId}-{$i}";
        $ids[$id] = true;
        $messages[] = message($queue, $runId, $id, PUBLISH_TIMEOUT_MS);
    }

    $pool->publishBatch($messages);
    $pool->flush();

    $errors = $pool->drainErrors();
    if ($errors !== []) {
        fail('publish errors after flush: '.json_encode($errors));
    }

    $confirmations = $pool->stats()['confirmations_total'] ?? 0;
    if ($confirmations < $count) {
        fail("expected >= {$count} confirmations, stats reports {$confirmations}");
    }
    pass("direct: {$count} messages published and confirmed (confirmations_total = {$confirmations}, no errors)");

    $duplicates = consumeAll($pool, $queue, $ids, 'direct', $runId);
    if ($duplicates !== 0) {
        fail("clean-path delivery produced {$duplicates} duplicate(s); the happy path must be exactly-once");
    }

    pass('direct: queue drained to depth 0');
}

/**
 * Phase B: through a private Toxiproxy proxy — warm up, disable the proxy,
 * publish 2 into the outage (must buffer in the bounded replay buffer),
 * re-enable, both must confirm within the deadline and be delivered. Any
 * loss or terminal publication error exits non-zero.
 */
function phaseRecovery(string $api, string $brokerHost, string $vhost, string $user, string $password, string $runId): void
{
    $queue = "bench.smoke-rec-{$runId}";
    $proxy = createRecoveryProxy($api);
    pass("recovery: proxy {$proxy['name']} listening on port {$proxy['port']}");

    $pool = new Pool(poolConfig($brokerHost, $proxy['port'], $vhost, $user, $password, $queue));

    try {
        // Warmup through the proxy: establishes the connection and proves
        // the pool's path crosses the proxy before any outage is injected.
        $warmup = message($queue, $runId, "smoke-warmup-{$runId}", PUBLISH_TIMEOUT_MS);
        $pool->publish($warmup);
        $pool->flush();
        if ($pool->drainErrors() !== []) {
            fail('warmup through the recovery proxy did not confirm');
        }
        $reconnectsBefore = $pool->stats()['reconnects_total'] ?? 0;
        consumeAll($pool, $queue, [$warmup['message_id'] => true], 'recovery warmup', $runId);

        // Cut the wire: disabling the proxy closes active connections and
        // refuses new ones until re-enabled.
        proxyToggle($api, $proxy['name'], $proxy['listen'], $proxy['upstream'], false);

        // Publish into the outage: both must be accepted into the bounded
        // replay buffer (never dropped), then confirmed after the heal. A
        // retry reuses the same message_id (duplicates are permitted).
        $outageIds = [];
        foreach (['rec-1', 'rec-2'] as $tag) {
            $id = "smoke-{$runId}-{$tag}";
            $attempts = 0;
            while (true) {
                try {
                    $pool->publish(message($queue, $runId, $id, RECOVERY_PUBLISH_TIMEOUT_MS));
                    $outageIds[$id] = true;
                    break;
                } catch (Throwable $e) {
                    $attempts++;
                    if ($attempts >= 10) {
                        fail("publish during outage never accepted for {$id}: ".$e->getMessage());
                    }
                    usleep(200000);
                }
            }
        }

        // Non-vacuous outage: the publications must actually be parked in
        // the replay buffer while the network is down.
        $buffered = 0;
        for ($i = 0; $i < 20; $i++) {
            $buffered = $pool->stats()['publish_buffered'] ?? 0;
            if ($buffered >= 2) {
                break;
            }
            usleep(100000);
        }
        if ($buffered < 2) {
            fail("expected the 2 outage publications to be buffered (publish_buffered = {$buffered}); "
                .'the at-least-once replay path was not exercised');
        }
        pass('recovery: 2 publications buffered during the outage (publish_buffered >= 2)');

        proxyToggle($api, $proxy['name'], $proxy['listen'], $proxy['upstream'], true);

        // Both publications must confirm within the deadline. The pool
        // reconnects automatically; a publication whose deadline expired
        // while parked is re-armed once before failing terminally — a
        // terminal failure surfaces in drainErrors() below as a loss.
        $deadline = microtime(true) + RECOVERY_DEADLINE_SECONDS;
        $lastError = '';
        while (true) {
            try {
                $pool->flush();
            } catch (Throwable $e) {
                $lastError = $e->getMessage();
            }

            $errors = $pool->drainErrors();
            if ($errors !== []) {
                fail('publications failed terminally after recovery (loss): '.json_encode($errors));
            }

            if (($pool->stats()['confirmations_total'] ?? 0) >= 3) { // warmup + 2
                break;
            }

            if (microtime(true) > $deadline) {
                fail('publications did not confirm within '.RECOVERY_DEADLINE_SECONDS.'s after recovery'
                    .($lastError === '' ? '' : "; last flush error: {$lastError}"));
            }
            usleep(500000);
        }

        if (($pool->stats()['publish_buffered'] ?? -1) !== 0) {
            fail('publish buffer did not quiesce to 0 after the heal; replay drain incomplete');
        }

        $duplicates = consumeAll($pool, $queue, $outageIds, 'recovery', $runId);
        pass('recovery: both outage publications confirmed + delivered (reconnects_total = '
            .($pool->stats()['reconnects_total'] ?? '?').", was {$reconnectsBefore}, duplicates = {$duplicates})");
    } finally {
        try {
            $pool->close();
        } catch (Throwable) {
            // best-effort cleanup; must not mask the outcome
        }
        toxiproxyRequest($api, 'DELETE', '/proxies/'.$proxy['name']);
    }
}

/**
 * @return array{0: string, 1: int}
 */
function parseDsn(string $dsn): array
{
    $parts = explode(':', $dsn);
    if (count($parts) !== 2 || $parts[1] === '' || ! ctype_digit($parts[1])) {
        fail("invalid --dsn '{$dsn}'; expected <host:port>");
    }

    return [$parts[0], (int) $parts[1]];
}

function main(array $argv): void
{
    $args = parseArgs($argv);

    if (! extension_loaded('rabbit_rs')) {
        fail('the rabbit_rs extension is not loaded (run with: php -d extension=<artifact> run.php ...)');
    }
    pass('extension loaded: rabbit_rs '.phpversion('rabbit_rs'));

    [$host, $port] = parseDsn($args['dsn'] ?? '127.0.0.1:5672');
    $api = rtrim($args['toxiproxy'] ?? 'http://127.0.0.1:18474', '/');
    $vhost = getenv('RABBIT_RS_SMOKE_VHOST') ?: VHOST_DEFAULT;
    $user = getenv('RABBIT_RS_SMOKE_USER') ?: BROKER_USER_DEFAULT;
    $password = getenv('RABBIT_RS_SMOKE_PASSWORD') ?: BROKER_PASSWORD_DEFAULT;

    $runId = bin2hex(random_bytes(4));
    $queue = "bench.smoke-{$runId}";

    // Phase A: direct path.
    $pool = new Pool(poolConfig($host, $port, $vhost, $user, $password, $queue));
    phaseDirect($pool, $queue, $runId, 5);
    $pool->close();
    unset($pool);

    // Phase B: outage + recovery through a private Toxiproxy proxy.
    if (isset($args['skip-recovery'])) {
        pass('recovery phase skipped (--skip-recovery)');
    } else {
        assertLabToxiproxy($api);
        phaseRecovery($api, $host, $vhost, $user, $password, $runId);
    }

    pass('ALL PHASES PASSED');
}

try {
    main($argv);
} catch (\Goopil\RabbitRs\ConnectionException $e) {
    fail('connection failure: '.$e->getMessage()
        .' — check broker reachability (DSN), vhost, and credentials'
        .' (RABBIT_RS_SMOKE_VHOST/_USER/_PASSWORD)');
} catch (Throwable $e) {
    fail(get_class($e).': '.$e->getMessage());
}
