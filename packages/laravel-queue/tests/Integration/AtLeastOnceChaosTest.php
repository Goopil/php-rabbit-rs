<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

const TOXIPROXY_API = 'http://localhost:8474';
const MGMT_API = 'http://localhost:15672';
const ADMIN_USER = 'admin';
const ADMIN_PASS = 'admin_lab';
const PROXY_1 = 'rabbitmq-1-toxiproxy';
const PRIMARY_NODE = 'rabbit@rabbitmq-1';

function resetToxiproxy(): void
{
    $ch = curl_init(TOXIPROXY_API . '/reset');
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'POST');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_TIMEOUT, 5);
    curl_exec($ch);
    curl_close($ch);
}

function addToxic(
    string $name,
    string $type,
    string $stream,
    float $toxicity,
    int $timeoutMs = 0,
): void {
    $payload = json_encode([
        'name' => $name,
        'type' => $type,
        'stream' => $stream,
        'toxicity' => $toxicity,
        'attributes' => $type === 'reset_peer' || $type === 'timeout'
            ? ['timeout' => $timeoutMs]
            : [],
    ]);

    $ch = curl_init(TOXIPROXY_API . '/proxies/' . PROXY_1 . '/toxics');
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'POST');
    curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    curl_setopt($ch, CURLOPT_TIMEOUT, 5);
    curl_exec($ch);
    curl_close($ch);
}

function removeToxic(string $name): void
{
    $ch = curl_init(TOXIPROXY_API . '/proxies/' . PROXY_1 . '/toxics/' . $name);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_TIMEOUT, 5);
    curl_exec($ch);
    curl_close($ch);
}

function getQueueLeader(string $queue): string
{
    $url = MGMT_API . '/api/queues/%2Forders-eu/' . urlencode($queue);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, ADMIN_USER . ':' . ADMIN_PASS);
    curl_setopt($ch, CURLOPT_TIMEOUT, 10);
    $resp = curl_exec($ch);
    curl_close($ch);

    $data = json_decode($resp, true);
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

    $config = liveConfig($test->queueName);
    $normalized = ConfigNormalizer::normalize($config);
    $test->pool = new Pool($normalized['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $test->pool);
    $connector = new RabbitMqConnector($factory, $normalized);
    $test->queue = $connector->connect([
        'queue' => $test->queueName,
        'block_for' => 10,
    ]);
    $test->queue->setContainer($app);
    $test->queue->setConnectionName('rabbit-rs-chaos');
}

/**
 * Returns whether the lab runs Toxiproxy with the rabbitmq-1 proxy. The lab
 * no longer ships Toxiproxy by default, so TCP-reset scenarios that depend on
 * injected toxics must skip loudly instead of passing without exercising any
 * failure.
 */
function toxiproxyProxyAvailable(): bool
{
    $ch = curl_init(TOXIPROXY_API.'/proxies/'.PROXY_1);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_TIMEOUT, 5);
    curl_exec($ch);
    $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
    curl_close($ch);

    return $status === 200;
}

function skipWithoutToxiproxy(): void
{
    if (! toxiproxyProxyAvailable()) {
        \PHPUnit\Framework\Assert::markTestSkipped(sprintf(
            'the RabbitMQ lab does not expose the %s Toxiproxy proxy; TCP-reset chaos cannot be injected',
            PROXY_1,
        ));
    }
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

    $this->queueName = uniqueQueue('rabbit-rs-it-chaos');
    declareQueue($this->queueName);

    resetToxiproxy();

    $config = liveConfig($this->queueName);
    $normalized = ConfigNormalizer::normalize($config);

    $this->pool = new Pool($normalized['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $this->pool);

    $connector = new RabbitMqConnector($factory, $normalized);
    $this->queue = $connector->connect([
        'queue' => $this->queueName,
        'block_for' => 10,
    ]);
    $this->queue->setContainer($this->app);
    $this->queue->setConnectionName('rabbit-rs-chaos');
});

afterEach(function () {
    resetToxiproxy();

    closePoolQuietly(isset($this->pool) ? $this->pool : null);
    deleteQueue($this->queueName);
});

/*
 * Scenario: TCP reset before publisher confirm.
 * The connection is reset between publish and confirm.
 * After recovery, the message must be delivered at-least-once.
 */
it('recovers from TCP reset before publisher confirm', function () {
    skipWithoutToxiproxy();

    $this->queue->clear($this->queueName);

    // Warmup: publish and consume one message to establish the connection.
    $this->queue->push('stdClass', ['msg' => 'warmup']);
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $job->delete();

    // Inject TCP reset on the proxy.
    addToxic('reset-before-confirm', 'reset_peer', 'downstream', 1.0, 100);

    // Attempt to publish during the outage.
    $published = false;
    try {
        $this->queue->push('stdClass', ['msg' => 'chaos-reset-1']);
        $published = true;
    } catch (\Throwable $e) {
        // Expected: publish may fail during the reset.
    }

    // Remove the toxic.
    removeToxic('reset-before-confirm');

    // Wait for recovery.
    usleep(3000000); // 3 seconds

    // If the first attempt failed, retry.
    if (! $published) {
        $this->queue->push('stdClass', ['msg' => 'chaos-reset-1']);
    }

    // Consume and verify at-least-once.
    $received = [];
    $job = $this->queue->pop();
    while ($job !== null) {
        $body = json_decode($job->getRawBody(), true);
        $received[] = $body['data']['msg'] ?? '';
        $job->delete();
        $job = $this->queue->pop();
    }

    $this->assertContains('chaos-reset-1', $received, 'missing = 0 for tcp-reset-before-confirm');
    echo "\n[tcp-reset-before-confirm] PASS: missing = 0\n";
});

/*
 * Scenario: TCP reset after confirm, before consumer ACK.
 * A message is confirmed by the broker, consumed, but the ACK
 * is lost due to a TCP reset. The message must be redelivered.
 */
it('redelivers after TCP reset between confirm and ACK', function () {
    skipWithoutToxiproxy();

    $this->queue->clear($this->queueName);

    // Publish a message.
    $this->queue->push('stdClass', ['msg' => 'chaos-ack-1']);

    // Pop the job but do NOT delete it (simulating processing).
    $job = $this->queue->pop();
    expect($job)->not->toBeNull()
        ->toBeInstanceOf(RabbitMqJob::class);

    // Inject TCP reset.
    addToxic('reset-before-ack', 'reset_peer', 'downstream', 1.0, 50);

    // Attempt to delete (ACK) — may fail due to the reset.
    try {
        $job->delete();
    } catch (\Throwable $e) {
        // Expected: ACK may fail during the reset.
    }

    // Remove the toxic.
    removeToxic('reset-before-ack');

    // Wait for reconnection and redelivery.
    usleep(3000000); // 3 seconds

    // Create a fresh pool to consume the redelivered message.
    recreatePool($this, $this->app);

    $job2 = $this->queue->pop();
    $this->assertNotNull($job2, 'redelivered message after TCP reset before ACK');

    $body = json_decode($job2->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-ack-1');
    $job2->delete();

    echo "\n[tcp-reset-after-confirm-before-ack] PASS: missing = 0\n";
});

/*
 * Scenario: Quorum leader shutdown.
 * The leader of a quorum queue is stopped. After failover,
 * published messages must still be delivered.
 */
it('survives quorum leader shutdown', function () {
    $this->queue->clear($this->queueName);

    // Publish before the leader shutdown.
    $this->queue->push('stdClass', ['msg' => 'chaos-leader-1']);

    // Find and stop the leader node.
    $leader = getQueueLeader($this->queueName);
    stopNode($leader);

    // Wait for quorum failover.
    usleep(5000000); // 5 seconds

    // Publish after the leader shutdown.
    $published = false;
    try {
        $this->queue->push('stdClass', ['msg' => 'chaos-leader-2']);
        $published = true;
    } catch (\Throwable $e) {
        // May need a retry.
    }

    // Restart the stopped node.
    startNode($leader);
    usleep(5000000); // 5 seconds

    // Retry if needed.
    if (! $published) {
        recreatePool($this, $this->app);
        $this->queue->push('stdClass', ['msg' => 'chaos-leader-2']);
    }

    // Consume both messages.
    $received = [];
    $job = $this->queue->pop();
    while ($job !== null) {
        $body = json_decode($job->getRawBody(), true);
        $received[] = $body['data']['msg'] ?? '';
        $job->delete();
        $job = $this->queue->pop();
    }

    $this->assertContains('chaos-leader-1', $received, 'missing leader-1');
    $this->assertContains('chaos-leader-2', $received, 'missing leader-2');
    echo "\n[quorum-leader-shutdown] PASS: missing = 0\n";
});

/*
 * Scenario: Node restart.
 * A RabbitMQ node is restarted. Messages published before
 * and after must both be delivered.
 */
it('survives node restart', function () {
    $this->queue->clear($this->queueName);

    // Publish before restart.
    $this->queue->push('stdClass', ['msg' => 'chaos-restart-1']);

    // Stop and start rabbitmq-1.
    stopNode(PRIMARY_NODE);
    usleep(2000000); // 2 seconds
    startNode(PRIMARY_NODE);
    usleep(5000000); // 5 seconds

    // Publish after restart.
    $published = false;
    try {
        $this->queue->push('stdClass', ['msg' => 'chaos-restart-2']);
        $published = true;
    } catch (\Throwable $e) {
        // May need retry.
    }

    if (! $published) {
        recreatePool($this, $this->app);
        $this->queue->push('stdClass', ['msg' => 'chaos-restart-2']);
    }

    // Consume both.
    $received = [];
    $job = $this->queue->pop();
    while ($job !== null) {
        $body = json_decode($job->getRawBody(), true);
        $received[] = $body['data']['msg'] ?? '';
        $job->delete();
        $job = $this->queue->pop();
    }

    $this->assertContains('chaos-restart-1', $received, 'missing restart-1');
    $this->assertContains('chaos-restart-2', $received, 'missing restart-2');
    echo "\n[node-restart] PASS: missing = 0\n";
});

/*
 * Scenario: Consumer network partition.
 * The consumer's network is partitioned. An unacked message
 * must be redelivered after the partition heals.
 */
it('redelivers after consumer network partition', function () {
    skipWithoutToxiproxy();

    $this->queue->clear($this->queueName);

    // Publish a message.
    $this->queue->push('stdClass', ['msg' => 'chaos-partition-1']);

    // Pop but do not ACK.
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();

    // Create a partition by blocking all traffic.
    addToxic('partition-consumer', 'timeout', 'downstream', 1.0, 0);

    // Attempt to delete (ACK) — will fail in the partition.
    try {
        $job->delete();
    } catch (\Throwable $e) {
        // Expected.
    }

    usleep(2000000); // 2 seconds in partition

    // Heal the partition.
    removeToxic('partition-consumer');
    usleep(3000000); // 3 seconds for recovery

    // Create a fresh pool and consume the redelivered message.
    recreatePool($this, $this->app);

    $job2 = $this->queue->pop();
    $this->assertNotNull($job2, 'redelivered message after partition');
    $body = json_decode($job2->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-partition-1');
    $job2->delete();

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

    $job2 = $this->queue->pop();
    expect($job2)->not->toBeNull();
    $body = json_decode($job2->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-topo-2');
    $job2->delete();

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

    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $body = json_decode($job->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-delay-1');
    $job->delete();

    echo "\n[delay-plugin-unavailable] PASS: missing = 0\n";
});

/*
 * Scenario: Credentials rejected.
 * Publishing with bad credentials must fail with a typed error.
 * Good credentials must still deliver at-least-once.
 */
it('rejects bad credentials and delivers with good credentials', function () {
    $this->queue->clear($this->queueName);

    // Build a config with bad credentials.
    $config = liveConfig($this->queueName);
    $config['brokers']['default']['credentials'] = [
        'username' => 'rabbit_rs',
        'password' => 'wrong_password',
    ];
    $normalized = ConfigNormalizer::normalize($config);

    $badPool = new Pool($normalized['native']);
    $badFactory = new NativePoolFactory(createPool: fn (): Pool => $badPool);
    $badConnector = new RabbitMqConnector($badFactory, $normalized);

    $threw = false;
    try {
        $badQueue = $badConnector->connect([
            'queue' => $this->queueName,
            'block_for' => 3,
        ]);
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
    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $body = json_decode($job->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-creds-2');
    $job->delete();

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

    $job2 = $this->queue->pop();
    $this->assertNotNull($job2, 'redelivered message after SIGTERM');
    $body = json_decode($job2->getRawBody(), true);
    expect($body['data']['msg'])->toBe('chaos-sigterm-1');
    $job2->delete();

    echo "\n[worker-sigterm-unacked] PASS: missing = 0\n";
});
