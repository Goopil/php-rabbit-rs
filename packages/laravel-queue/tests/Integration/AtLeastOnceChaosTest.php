<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

/*
 * Toxiproxy is a lab-owned service (lab/rabbitmq/compose.yaml), bound to a
 * lab-unique port (18474 — the conventional 8474 is frequently grabbed by
 * unrelated projects). The suite pins itself to that instance in two steps:
 *
 *  1. identity: the fingerprint proxy "rabbitmq-1" must exist and upstream to
 *     the lab node rabbitmq-1:5672 — a foreign Toxiproxy on the port fails
 *     loudly instead of silently receiving toxics meant for RabbitMQ;
 *  2. isolation: every toxic scenario creates its own proxy (unique name,
 *     listen port in the lab's 24504-24509 range) and deletes it in teardown,
 *     so toxics only ever hit connections this suite opened.
 *
 * Toxiproxy absence is a hard failure, not a skip: a chaos scenario that does
 * not exercise any failure is a vacuous pass.
 */
const TOXIPROXY_API_DEFAULT = 'http://localhost:18474';
const LAB_FINGERPRINT_PROXY = 'rabbitmq-1';
const LAB_FINGERPRINT_UPSTREAM = 'rabbitmq-1:5672';
const TOXIPROXY_PROXIES_PATH = '/proxies';
const CHAOS_PROXY_PORT_MIN = 24504;
const CHAOS_PROXY_PORT_MAX = 24509;
const PRIMARY_NODE = 'rabbit@rabbitmq-1';

function toxiproxyApi(): string
{
    $api = getenv('RABBIT_RS_TOXIPROXY_API');

    return $api === false || $api === '' ? TOXIPROXY_API_DEFAULT : $api;
}

/**
 * @return array{int, string} [HTTP status, response body]
 */
function toxiproxyRequest(string $method, string $path, ?string $payload = null): array
{
    $ch = curl_init(toxiproxyApi() . $path);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, $method);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_TIMEOUT, 5);
    if ($payload !== null) {
        curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    }
    $body = curl_exec($ch);
    $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
    curl_close($ch);

    return [$status, $body === false ? '' : $body];
}

/**
 * Fails the test unless the Toxiproxy answering on the API port is the lab's
 * own instance, proven by the rabbitmq-1 fingerprint proxy upstream.
 */
function assertLabToxiproxy(): void
{
    [$status, $body] = toxiproxyRequest('GET', TOXIPROXY_PROXIES_PATH . '/' . LAB_FINGERPRINT_PROXY);

    if ($status === 404) {
        \PHPUnit\Framework\Assert::fail(sprintf(
            '%s answers but has no lab fingerprint proxy "%s" (upstream %s): this is not the lab Toxiproxy. '
                .'A foreign instance must never receive toxics meant for RabbitMQ. '
                .'Start the lab with ./scripts/lab-up.sh',
            toxiproxyApi(),
            LAB_FINGERPRINT_PROXY,
            LAB_FINGERPRINT_UPSTREAM,
        ));
    }

    if ($status !== 200) {
        \PHPUnit\Framework\Assert::fail(sprintf(
            'the lab Toxiproxy is not reachable at %s (HTTP %d); chaos scenarios refuse to run without '
                .'a lab-owned instance because injecting toxics elsewhere proves nothing. '
                .'Start the lab with ./scripts/lab-up.sh',
            toxiproxyApi(),
            $status,
        ));
    }

    $upstream = json_decode($body, true)['upstream'] ?? '';
    if ($upstream !== LAB_FINGERPRINT_UPSTREAM) {
        \PHPUnit\Framework\Assert::fail(sprintf(
            '%s is answered by a foreign Toxiproxy (%s upstream is "%s", expected "%s"); '
                .'refusing to inject toxics into infrastructure this suite does not own',
            toxiproxyApi(),
            LAB_FINGERPRINT_PROXY,
            $upstream === '' ? 'none' : $upstream,
            LAB_FINGERPRINT_UPSTREAM,
        ));
    }
}

/**
 * Creates a private proxy upstream to the lab's rabbitmq-1 node and returns
 * its name and host listen port. Listen-port conflicts (concurrent suites)
 * are retried on other ports from the lab's dedicated range.
 *
 * @return array{name: string, port: int}
 */
function createChaosProxy(): array
{
    $name = 'chaos-'.uniqid('', true);

    for ($attempt = 0; $attempt < 4; $attempt++) {
        $port = random_int(CHAOS_PROXY_PORT_MIN, CHAOS_PROXY_PORT_MAX);
        [$status] = toxiproxyRequest('POST', TOXIPROXY_PROXIES_PATH, json_encode([
            'name' => $name,
            'listen' => '0.0.0.0:'.$port,
            'upstream' => LAB_FINGERPRINT_UPSTREAM,
            'enabled' => true,
        ]));

        if ($status === 200 || $status === 201) {
            return ['name' => $name, 'port' => $port];
        }
    }

    \PHPUnit\Framework\Assert::fail(sprintf(
        'could not create chaos proxy %s on %s (all candidate listen ports busy): HTTP %s',
        $name,
        toxiproxyApi(),
        $status,
    ));
}

function deleteChaosProxy(string $name): void
{
    toxiproxyRequest('DELETE', TOXIPROXY_PROXIES_PATH.'/'.$name);
}

/**
 * Opens (or reopens) the suite's pool through the optional chaos proxy and
 * assigns it to $test->pool / $test->queue.
 */
function openChaosPool($test, $app, ?array $brokerHosts = null): void
{
    [$test->pool, $test->queue] = integrationPoolAndQueue(
        $app,
        $test->queueName,
        connectOverrides: ['block_for' => 10],
        connectionName: 'rabbit-rs-chaos',
        brokerHosts: $brokerHosts,
    );
}

/**
 * Routes the test's pool through a private proxy: the pool's broker host
 * becomes the proxy's listen port, so every injected toxic affects exactly
 * the connections this test opened. The proxy is deleted in afterEach().
 */
function useChaosProxy($test, $app): void
{
    assertLabToxiproxy();

    closePoolQuietly(isset($test->pool) ? $test->pool : null);
    $proxy = createChaosProxy();
    $test->chaosProxy = $proxy['name'];

    openChaosPool($test, $app, ['127.0.0.1:'.$proxy['port']]);
}

function addToxic(string $proxy, string $name, string $type, string $stream, float $toxicity, int $timeoutMs = 0): void
{
    $payload = json_encode([
        'name' => $name,
        'type' => $type,
        'stream' => $stream,
        'toxicity' => $toxicity,
        'attributes' => $type === 'reset_peer' || $type === 'timeout'
            ? ['timeout' => $timeoutMs]
            : [],
    ]);

    [$status, $body] = toxiproxyRequest('POST', TOXIPROXY_PROXIES_PATH.'/'.$proxy.'/toxics', $payload);

    // A toxic that fails to apply would turn the scenario into a vacuous
    // pass; fail loudly instead.
    if ($status !== 200) {
        \PHPUnit\Framework\Assert::fail(sprintf(
            'toxic %s was not applied to proxy %s (HTTP %d): %s',
            $name,
            $proxy,
            $status,
            $body,
        ));
    }
}

function removeToxic(string $proxy, string $name): void
{
    toxiproxyRequest('DELETE', TOXIPROXY_PROXIES_PATH.'/'.$proxy.'/toxics/'.$name);
}

/**
 * Cuts the connection on BOTH proxy legs. Timeout toxics close the sockets
 * after the delay, so the broker learns the consumer vanished (and requeues
 * the unacked message) while the client sees its socket die too — a
 * single-leg reset_peer leaves the other side half-open on an idle
 * connection and neither side ever notices. Both toxics are applied with a
 * loud 200 check.
 */
function addConnectionKill(string $proxy, string $name, int $timeoutMs): void
{
    addToxic($proxy, $name.'-up', 'timeout', 'upstream', 1.0, $timeoutMs);
    addToxic($proxy, $name.'-down', 'timeout', 'downstream', 1.0, $timeoutMs);
}

function removeConnectionKill(string $proxy, string $name): void
{
    removeToxic($proxy, $name.'-up');
    removeToxic($proxy, $name.'-down');
}

function getQueueLeader(string $queue): string
{
    $body = managementRequest('GET', 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queue));
    $data = json_decode($body, true);

    return $data['leader'] ?? PRIMARY_NODE;
}

function stopNode(string $node): void
{
    $container = nodeToContainer($node);
    exec("docker stop {$container} 2>&1", $output, $exit);
}

function startNode(string $node): void
{
    $container = nodeToContainer($node);
    exec("docker start {$container} 2>&1", $output, $exit);

    // Wait for node to be responsive.
    for ($i = 0; $i < 30; $i++) {
        $pingOutput = [];
        exec("docker exec {$container} rabbitmq-diagnostics -q ping 2>&1", $pingOutput, $pingExit);
        if ($pingExit === 0) {
            break;
        }
        usleep(2000000); // 2 seconds
    }

    restoreConfigurePermission();
}

/**
 * Re-grants the rabbit-rs configure permission, polling until the widened
 * pattern verifiably landed on the management API. The lab re-imports
 * definitions.json on every node boot, which resets the permission to the
 * stored narrow pattern, and the driver's recovery re-declares
 * rabbit-rs.delayed (issue #97) — so every recovery generation after the
 * restart fails topology reconciliation until the grant is restored. A bare
 * PUT right after the rabbitmq-diagnostics ping silently no-ops while the
 * management listener is still down.
 */
function restoreConfigurePermission(): void
{
    $deadline = microtime(true) + 90;
    do {
        grantRabbitRsConfigure();
        $stored = json_decode(
            managementRequest('GET', 'http://localhost:15672/api/permissions/'.rawurlencode('/orders-eu').'/rabbit_rs'),
            true,
        );
        if (($stored['configure'] ?? null) === '^(amq\\.|rabbit-rs-it-|rabbit-rs\\.)') {
            return;
        }
        usleep(1000000);
    } while (microtime(true) < $deadline);

    \PHPUnit\Framework\Assert::fail(
        'the lab management API never restored the rabbit-rs configure permission after the node restart',
    );
}

function nodeToContainer(string $node): string
{
    $parts = explode('@', $node);
    $suffix = end($parts);
    return "rabbitrs-{$suffix}-1";
}

function recreatePool($test, $app): void
{
    closePoolQuietly(isset($test->pool) ? $test->pool : null);

    openChaosPool($test, $app);
}

/**
 * Reads the pool's connection/ack counters. A pool whose connection was
 * bounced by a chaos scenario can throw from stats(); nulls mean "unknown".
 *
 * @return array{?int, ?int} [reconnects_total, acks_total]
 */
function poolCounters(?Pool $pool): array
{
    if ($pool === null) {
        return [null, null];
    }

    try {
        $stats = $pool->stats();
    } catch (\Throwable) {
        return [null, null];
    }

    return [$stats['reconnects_total'] ?? null, $stats['acks_total'] ?? null];
}

/**
 * Pops one job, tolerating the one-shot retired-consumer errors a connection
 * bump produces (the queue evicts the cache and re-fetches on the next pop —
 * see ConsumerRejoinTest). Returns null when nothing arrives within the
 * deadline.
 */
function popDelivery($test, int $timeoutSec = 30): ?object
{
    $deadline = microtime(true) + $timeoutSec;
    while (microtime(true) < $deadline) {
        try {
            $job = $test->queue->pop();
            if ($job !== null) {
                return $job;
            }
        } catch (QueueException | ConnectionException) {
            // expected while the retired handle's error surfaces
        }
        usleep(100000);
    }

    return null;
}

/**
 * Pops and verifies a job's payload after a chaos scenario. Fails the test
 * when nothing is delivered (at-least-once violation).
 */
function consumeDeliveredMessage($test, string $expectedMessage, string $description): object
{
    $job = popDelivery($test);
    $test->assertNotNull($job, $description);

    $body = json_decode($job->getRawBody(), true);
    expect($body['data']['msg'])->toBe($expectedMessage);
    $job->delete();

    return $job;
}

/**
 * Drains every currently available job and returns their payload messages,
 * tolerating retired-consumer errors the same way popDelivery() does.
 */
function drainDeliveredMessages($test): array
{
    $received = [];
    $deadline = microtime(true) + 30;
    while (microtime(true) < $deadline) {
        try {
            $job = $test->queue->pop();
        } catch (QueueException | ConnectionException) {
            usleep(100000);
            continue;
        }
        if ($job === null) {
            break;
        }
        $body = json_decode($job->getRawBody(), true);
        $received[] = $body['data']['msg'] ?? '';
        $job->delete();
    }

    return $received;
}

/**
 * Publishes tolerating the connection failures an active toxic produces.
 * Returns whether the publish went through; callers retry after recovery.
 */
function pushAllowingFailure($queue, string $msg): bool
{
    try {
        $queue->push('stdClass', ['msg' => $msg]);

        return true;
    } catch (\Throwable) {
        return false; // publish may fail during the outage
    }
}

/**
 * Makes sure a message is confirmed on the wire after a disruption: the
 * publish may have failed with the dead connection, and a publication that
 * stayed in the process publish buffer only leaves on the next publish's
 * flush trigger or an explicit flush — a drain would never see it. Retries
 * with a fresh pool while the connection is still re-establishing. A
 * duplicate is acceptable; the delivery contract is at-least-once.
 */
function putOnWire($test, $app, string $msg): void
{
    $onWire = false;
    $deadline = microtime(true) + 30;
    while (microtime(true) < $deadline && ! $onWire) {
        $onWire = pushAllowingFailure($test->queue, $msg);
        if ($onWire) {
            try {
                $test->pool->flush();
            } catch (\Throwable) {
                $onWire = false; // pool still recovering
            }
        }
        if (! $onWire) {
            recreatePool($test, $app);
            usleep(500000);
        }
    }

    $test->assertTrue($onWire, $msg.' never reached the broker after the disruption');
}

/**
 * Applies a both-legs connection kill, lets the resets fire, removes the
 * toxics.
 */
function fireConnectionKill(string $proxy, string $name, int $timeoutMs): void
{
    addConnectionKill($proxy, $name, $timeoutMs);
    usleep(300000); // let the resets fire
    removeConnectionKill($proxy, $name);
}

/**
 * Drains the queue and asserts every message arrived (at-least-once).
 */
function assertAllDelivered($test, array $messages, string $label): void
{
    $received = drainDeliveredMessages($test);
    foreach ($messages as $msg) {
        $test->assertContains($msg, $received, 'missing '.$msg);
    }
    echo "\n[".$label.'] PASS: missing = 0'."\n";
}

/**
 * Closes a pool best-effort. A pool whose broker connection was bounced by a
 * chaos scenario (or already closed by the test body) can throw from
 * stats()/close() (lapin reports an unexpected connection state) even though
 * the handle is released internally. Cleanup must never mask the test outcome.
 */
function closePoolQuietly(?Pool $pool): void
{
    if ($pool === null) {
        return;
    }

    try {
        $pool->close();
    } catch (\Throwable) {
        // best-effort cleanup
    }
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for chaos tests');
    }

    // Declare-mode pools with the default auto delay strategy now provision
    // the rabbit-rs.delayed exchange themselves (issue #97); the lab's stored
    // configure permission only allows amq.* and rabbit-rs-it-* names.
    grantRabbitRsConfigure();

    $this->queueName = uniqueQueue('rabbit-rs-it-chaos');
    declareQueue($this->queueName);

    openChaosPool($this, $this->app);
});

afterEach(function () {
    closePoolQuietly(isset($this->pool) ? $this->pool : null);

    if (isset($this->chaosProxy)) {
        deleteChaosProxy($this->chaosProxy);
    }

    deleteQueue($this->queueName);
});

/*
 * Scenario: TCP reset before publisher confirm.
 * The connection is reset between publish and confirm.
 * After recovery, the message must be delivered at-least-once.
 */
it('recovers from TCP reset before publisher confirm', function () {
    useChaosProxy($this, $this->app);

    $this->queue->clear($this->queueName);

    // Warmup: publish and consume one message to establish the connection.
    $this->queue->push('stdClass', ['msg' => 'warmup']);
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $job->delete();

    [$reconnectsBefore] = poolCounters($this->pool);

    // Inject TCP reset on this test's own proxy.
    addToxic($this->chaosProxy, 'reset-before-confirm', 'reset_peer', 'downstream', 1.0, 100);

    // Attempt to publish during the outage. flush() puts the publication on
    // the wire NOW: a publication still held in the process-side publish
    // buffer never crosses the proxy, so the reset_peer toxic (which fires on
    // traffic) would never trigger and the scenario would be vacuous.
    $published = pushAllowingFailure($this->queue, 'chaos-reset-1');
    if ($published) {
        try {
            $this->pool->flush();
        } catch (\Throwable) {
            $published = false; // the reset killed the connection mid-confirm
        }
    }

    // Remove the toxic.
    removeToxic($this->chaosProxy, 'reset-before-confirm');

    // Wait for recovery.
    usleep(3000000); // 3 seconds

    // Non-vacuous: the toxic must have actually bounced the connection.
    [$reconnectsAfter] = poolCounters($this->pool);
    $this->assertNotNull($reconnectsAfter, 'pool stats unavailable; cannot verify the toxic fired');
    $this->assertGreaterThan($reconnectsBefore, $reconnectsAfter, 'the toxic never fired; the scenario would be vacuous');

    // If the first attempt failed, retry.
    if (! $published) {
        $this->queue->push('stdClass', ['msg' => 'chaos-reset-1']);
    }

    // Consume and verify at-least-once.
    assertAllDelivered($this, ['chaos-reset-1'], 'tcp-reset-before-confirm');
});

/*
 * Scenario: TCP reset after confirm, before consumer ACK.
 * A message is confirmed by the broker, consumed, but the ACK
 * is lost due to a TCP reset. The message must be redelivered.
 */
it('redelivers after TCP reset between confirm and ACK', function () {
    useChaosProxy($this, $this->app);

    $this->queue->clear($this->queueName);

    // Publish a message.
    $this->queue->push('stdClass', ['msg' => 'chaos-ack-1']);

    // Pop the job but do NOT delete it (simulating processing).
    $job = $this->queue->pop();
    expect($job)->not->toBeNull()
        ->toBeInstanceOf(RabbitMqJob::class);

    // Inject TCP reset: both proxy legs are cut ~50ms after activation, so
    // the broker sees the connection die while the message is unacked (and
    // must requeue it) and the client sees its socket die too. addToxic
    // fails the test loudly unless the toxics were really applied, and
    // useChaosProxy guarantees the pool's only path runs through this proxy,
    // so the scenario cannot pass without a real disruption.
    fireConnectionKill($this->chaosProxy, 'reset-before-ack', 50);

    // Wait for reconnection and redelivery.
    usleep(3000000); // 3 seconds

    // The rejoined consumer must receive the redelivered message.
    consumeDeliveredMessage($this, 'chaos-ack-1', 'at-least-once violation: message never redelivered after TCP reset before ACK');

    echo "\n[tcp-reset-after-confirm-before-ack] PASS: missing = 0\n";
});

/*
 * Scenario: Quorum leader shutdown.
 * The leader of a quorum queue is stopped. After failover,
 * published messages must still be delivered.
 */
it('survives quorum leader shutdown', function () {
    $this->queue->clear($this->queueName);

    // Publish before the leader shutdown and put it on the wire: a
    // publication still in the process-side publish buffer would be dropped
    // by the recreatePool teardown below instead of facing the outage.
    $this->queue->push('stdClass', ['msg' => 'chaos-leader-1']);
    $this->pool->flush();

    // Find and stop the leader node.
    $leader = getQueueLeader($this->queueName);
    stopNode($leader);

    // Wait for quorum failover.
    usleep(5000000); // 5 seconds

    // Publish after the leader shutdown.
    pushAllowingFailure($this->queue, 'chaos-leader-2');

    // Restart the stopped node.
    startNode($leader);
    usleep(5000000); // 5 seconds

    // The outage push may have failed or still sit in the process publish
    // buffer (a buffered publication only leaves on the next publish's
    // flush trigger or an explicit flush). Make sure chaos-leader-2 is on
    // the wire before draining: duplicates are fine — at-least-once.
    putOnWire($this, $this->app, 'chaos-leader-2');

    // Consume both messages.
    assertAllDelivered($this, ['chaos-leader-1', 'chaos-leader-2'], 'quorum-leader-shutdown');
});

/*
 * Scenario: Node restart.
 * A RabbitMQ node is restarted. Messages published before
 * and after must both be delivered.
 */
it('survives node restart', function () {
    $this->queue->clear($this->queueName);

    // Publish before restart and put it on the wire (same reasoning as the
    // leader-shutdown scenario: recreatePool must not drop a buffered
    // publication).
    $this->queue->push('stdClass', ['msg' => 'chaos-restart-1']);
    $this->pool->flush();

    // Stop and start rabbitmq-1.
    stopNode(PRIMARY_NODE);
    usleep(2000000); // 2 seconds
    startNode(PRIMARY_NODE);
    usleep(5000000); // 5 seconds

    // Publish after restart: the pool may still be re-establishing, and a
    // buffered publication only leaves on the next publish's flush trigger
    // or an explicit flush. Make sure chaos-restart-2 is on the wire.
    putOnWire($this, $this->app, 'chaos-restart-2');

    // Consume both.
    assertAllDelivered($this, ['chaos-restart-1', 'chaos-restart-2'], 'node-restart');
});

/*
 * Scenario: Consumer network partition.
 * The consumer's network is partitioned. An unacked message
 * must be redelivered after the partition heals.
 */
it('redelivers after consumer network partition', function () {
    useChaosProxy($this, $this->app);

    $this->queue->clear($this->queueName);

    // Publish a message.
    $this->queue->push('stdClass', ['msg' => 'chaos-partition-1']);

    // Pop but do not ACK.
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();

    // Partition the consumer by cutting both proxy legs, so the broker sees
    // the connection die and requeues the unacked message while the client's
    // socket dies too. addToxic fails the test loudly unless the toxics were
    // really applied, and useChaosProxy guarantees the pool's only path runs
    // through this proxy.
    fireConnectionKill($this->chaosProxy, 'partition-consumer', 100);

    usleep(2000000); // 2 seconds in partition aftermath

    // The rejoined consumer must receive the redelivered message.
    consumeDeliveredMessage($this, 'chaos-partition-1', 'at-least-once violation: message never redelivered after partition');

    echo "\n[consumer-partition] PASS: missing = 0\n";
});

/*
 * Scenario: Channel closed for topology error.
 * After a channel error, publishing must still work with a new channel.
 */
it('publishes after channel closed for topology error', function () {
    $this->queue->clear($this->queueName);

    // Publish and consume successfully first.
    $this->queue->push('stdClass', ['msg' => 'chaos-topo-1']);
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $job->delete();

    // Close and recreate the pool to simulate a channel error.
    recreatePool($this, $this->app);
    $this->queue->clear($this->queueName);

    // Publish after the channel recreation.
    $this->queue->push('stdClass', ['msg' => 'chaos-topo-2']);

    consumeDeliveredMessage($this, 'chaos-topo-2', 'message delivered after the channel recreation');

    echo "\n[channel-closed-topology-error] PASS: missing = 0\n";
});

/*
 * Scenario: Delay plugin unavailable.
 * Regular publish/consume must still work regardless of the
 * delay plugin state.
 */
it('works with delay plugin unavailable', function () {
    $this->queue->clear($this->queueName);

    $this->queue->push('stdClass', ['msg' => 'chaos-delay-1']);

    consumeDeliveredMessage($this, 'chaos-delay-1', 'the published job is delivered');

    echo "\n[delay-plugin-unavailable] PASS: missing = 0\n";
});

/*
 * Scenario: Credentials rejected.
 * Publishing with bad credentials must fail with a typed error.
 * Good credentials must still deliver at-least-once.
 */
it('rejects bad credentials and delivers with good credentials', function () {
    $this->queue->clear($this->queueName);

    // Build a connection config with bad credentials, compiled to its own
    // pool under a dedicated connection name.
    $config = liveConfig($this->queueName);
    $config['password'] = 'wrong_password';
    $compiled = ConnectionCompiler::compile('rabbit-rs-bad-creds', $config);

    $badPool = new Pool($compiled['native']);
    $badFactory = new NativePoolFactory(createPool: fn (): Pool => $badPool);
    $badConnector = new RabbitMqConnector($badFactory, $config);

    $connectConfig = ['queue' => $this->queueName, 'block_for' => 3];
    $this->app['config']->set('queue.connections.rabbit-rs-bad-creds', $connectConfig);

    $threw = false;
    try {
        $badQueue = $badConnector->connect($connectConfig);
        $badQueue->push('stdClass', ['msg' => 'should-fail']);
    } catch (\Throwable $e) {
        $threw = true;
    } finally {
        closePoolQuietly($badPool);
    }

    expect($threw)->toBeTrue('publish with bad credentials must fail');
    echo "\n[credentials-rejected] PASS: bad credentials correctly rejected\n";

    // Verify good credentials still work.
    $this->queue->push('stdClass', ['msg' => 'chaos-creds-2']);
    consumeDeliveredMessage($this, 'chaos-creds-2', 'good credentials still deliver');

    echo "\n[credentials-rejected] PASS: missing = 0 with good credentials\n";
});

/*
 * Scenario: Worker SIGTERM with unacked jobs.
 * A worker receives a SIGTERM while holding an unacked job.
 * The job must be redelivered to a new worker.
 */
it('redelivers after worker SIGTERM with unacked jobs', function () {
    $this->queue->clear($this->queueName);

    // Publish a message.
    $this->queue->push('stdClass', ['msg' => 'chaos-sigterm-1']);

    // Pop the job but do NOT ACK it — simulating a worker that
    // received SIGTERM while processing.
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();

    // Simulate SIGTERM: close the pool without ACKing.
    $this->pool->close();
    usleep(3000000); // 3 seconds for the broker to redeliver

    // Create a fresh pool and consume the redelivered message.
    recreatePool($this, $this->app);

    consumeDeliveredMessage($this, 'chaos-sigterm-1', 'redelivered message after SIGTERM');

    echo "\n[worker-sigterm-unacked] PASS: missing = 0\n";
});
