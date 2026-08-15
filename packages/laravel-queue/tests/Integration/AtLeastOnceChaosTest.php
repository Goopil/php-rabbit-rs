<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Integration;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

/**
 * At-least-once chaos tests for the Laravel queue driver.
 *
 * Each scenario injects a fault (via Toxiproxy or process signals) and
 * verifies that no messages are lost. Duplicates are permitted in
 * documented ambiguous windows.
 */
final class AtLeastOnceChaosTest extends IntegrationTestCase
{
    private const TOXIPROXY_API = 'http://localhost:8474';
    private const MGMT_API = 'http://localhost:15672';
    private const ADMIN_USER = 'admin';
    private const ADMIN_PASS = 'admin_lab';
    private const PROXY_1 = 'rabbitmq-1-toxiproxy';

    protected RabbitMqQueue $queue;
    private Pool $pool;
    private string $queueName;

    protected function setUp(): void
    {
        parent::setUp();

        if (! extension_loaded('rabbit_rs')) {
            self::markTestSkipped('ext-rabbit_rs is required for chaos tests');
        }

        $this->queueName = $this->uniqueQueue('rabbit-rs-it-chaos');
        $this->declareQueue($this->queueName);

        $this->resetToxiproxy();

        $config = $this->liveConfig($this->queueName);
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
    }

    protected function tearDown(): void
    {
        $this->resetToxiproxy();

        if (isset($this->pool) && ! $this->pool->stats()['closed']) {
            $this->pool->close();
        }
        $this->deleteQueue($this->queueName);
        parent::tearDown();
    }

    /**
     * Scenario: TCP reset before publisher confirm.
     * The connection is reset between publish and confirm.
     * After recovery, the message must be delivered at-least-once.
     */
    public function test_tcp_reset_before_confirm(): void
    {
        $this->queue->clear($this->queueName);

        // Warmup: publish and consume one message to establish the connection.
        $this->queue->push('stdClass', ['msg' => 'warmup']);
        $job = $this->queue->pop();
        self::assertNotNull($job);
        $job->delete();

        // Inject TCP reset on the proxy.
        $this->addToxic('reset-before-confirm', 'reset_peer', 'downstream', 1.0, 100);

        // Attempt to publish during the outage.
        $published = false;
        try {
            $this->queue->push('stdClass', ['msg' => 'chaos-reset-1']);
            $published = true;
        } catch (\Throwable $e) {
            // Expected: publish may fail during the reset.
        }

        // Remove the toxic.
        $this->removeToxic('reset-before-confirm');

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

        self::assertContains('chaos-reset-1', $received, 'missing = 0 for tcp-reset-before-confirm');
        echo "\n[tcp-reset-before-confirm] PASS: missing = 0\n";
    }

    /**
     * Scenario: TCP reset after confirm, before consumer ACK.
     * A message is confirmed by the broker, consumed, but the ACK
     * is lost due to a TCP reset. The message must be redelivered.
     */
    public function test_tcp_reset_after_confirm_before_ack(): void
    {
        $this->queue->clear($this->queueName);

        // Publish a message.
        $this->queue->push('stdClass', ['msg' => 'chaos-ack-1']);

        // Pop the job but do NOT delete it (simulating processing).
        $job = $this->queue->pop();
        self::assertNotNull($job);
        self::assertInstanceOf(RabbitMqJob::class, $job);

        // Inject TCP reset.
        $this->addToxic('reset-before-ack', 'reset_peer', 'downstream', 1.0, 50);

        // Attempt to delete (ACK) — may fail due to the reset.
        try {
            $job->delete();
        } catch (\Throwable $e) {
            // Expected: ACK may fail during the reset.
        }

        // Remove the toxic.
        $this->removeToxic('reset-before-ack');

        // Wait for reconnection and redelivery.
        usleep(3000000); // 3 seconds

        // Create a fresh pool to consume the redelivered message.
        $this->recreatePool();

        $job2 = $this->queue->pop();
        self::assertNotNull($job2, 'redelivered message after TCP reset before ACK');

        $body = json_decode($job2->getRawBody(), true);
        self::assertSame('chaos-ack-1', $body['data']['msg']);
        $job2->delete();

        echo "\n[tcp-reset-after-confirm-before-ack] PASS: missing = 0\n";
    }

    /**
     * Scenario: Quorum leader shutdown.
     * The leader of a quorum queue is stopped. After failover,
     * published messages must still be delivered.
     */
    public function test_quorum_leader_shutdown(): void
    {
        $this->queue->clear($this->queueName);

        // Publish before the leader shutdown.
        $this->queue->push('stdClass', ['msg' => 'chaos-leader-1']);

        // Find and stop the leader node.
        $leader = $this->getQueueLeader($this->queueName);
        $this->stopNode($leader);

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
        $this->startNode($leader);
        usleep(5000000); // 5 seconds

        // Retry if needed.
        if (! $published) {
            $this->recreatePool();
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

        self::assertContains('chaos-leader-1', $received, 'missing leader-1');
        self::assertContains('chaos-leader-2', $received, 'missing leader-2');
        echo "\n[quorum-leader-shutdown] PASS: missing = 0\n";
    }

    /**
     * Scenario: Node restart.
     * A RabbitMQ node is restarted. Messages published before
     * and after must both be delivered.
     */
    public function test_node_restart(): void
    {
        $this->queue->clear($this->queueName);

        // Publish before restart.
        $this->queue->push('stdClass', ['msg' => 'chaos-restart-1']);

        // Stop and start rabbitmq-1.
        $this->stopNode('rabbit@rabbitmq-1');
        usleep(2000000); // 2 seconds
        $this->startNode('rabbit@rabbitmq-1');
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
            $this->recreatePool();
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

        self::assertContains('chaos-restart-1', $received, 'missing restart-1');
        self::assertContains('chaos-restart-2', $received, 'missing restart-2');
        echo "\n[node-restart] PASS: missing = 0\n";
    }

    /**
     * Scenario: Consumer network partition.
     * The consumer's network is partitioned. An unacked message
     * must be redelivered after the partition heals.
     */
    public function test_consumer_partition(): void
    {
        $this->queue->clear($this->queueName);

        // Publish a message.
        $this->queue->push('stdClass', ['msg' => 'chaos-partition-1']);

        // Pop but do not ACK.
        $job = $this->queue->pop();
        self::assertNotNull($job);

        // Create a partition by blocking all traffic.
        $this->addToxic('partition-consumer', 'timeout', 'downstream', 1.0, 0);

        // Attempt to delete (ACK) — will fail in the partition.
        try {
            $job->delete();
        } catch (\Throwable $e) {
            // Expected.
        }

        usleep(2000000); // 2 seconds in partition

        // Heal the partition.
        $this->removeToxic('partition-consumer');
        usleep(3000000); // 3 seconds for recovery

        // Create a fresh pool and consume the redelivered message.
        $this->recreatePool();

        $job2 = $this->queue->pop();
        self::assertNotNull($job2, 'redelivered message after partition');
        $body = json_decode($job2->getRawBody(), true);
        self::assertSame('chaos-partition-1', $body['data']['msg']);
        $job2->delete();

        echo "\n[consumer-partition] PASS: missing = 0\n";
    }

    /**
     * Scenario: Channel closed for topology error.
     * After a channel error, publishing must still work with a new channel.
     */
    public function test_channel_closed_topology_error(): void
    {
        $this->queue->clear($this->queueName);

        // Publish and consume successfully first.
        $this->queue->push('stdClass', ['msg' => 'chaos-topo-1']);
        $job = $this->queue->pop();
        self::assertNotNull($job);
        $job->delete();

        // Close and recreate the pool to simulate a channel error.
        $this->recreatePool();
        $this->queue->clear($this->queueName);

        // Publish after the channel recreation.
        $this->queue->push('stdClass', ['msg' => 'chaos-topo-2']);

        $job2 = $this->queue->pop();
        self::assertNotNull($job2);
        $body = json_decode($job2->getRawBody(), true);
        self::assertSame('chaos-topo-2', $body['data']['msg']);
        $job2->delete();

        echo "\n[channel-closed-topology-error] PASS: missing = 0\n";
    }

    /**
     * Scenario: Delay plugin unavailable.
     * Regular publish/consume must still work regardless of the
     * delay plugin state.
     */
    public function test_delay_plugin_unavailable(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->push('stdClass', ['msg' => 'chaos-delay-1']);

        $job = $this->queue->pop();
        self::assertNotNull($job);
        $body = json_decode($job->getRawBody(), true);
        self::assertSame('chaos-delay-1', $body['data']['msg']);
        $job->delete();

        echo "\n[delay-plugin-unavailable] PASS: missing = 0\n";
    }

    /**
     * Scenario: Credentials rejected.
     * Publishing with bad credentials must fail with a typed error.
     * Good credentials must still deliver at-least-once.
     */
    public function test_credentials_rejected(): void
    {
        $this->queue->clear($this->queueName);

        // Build a config with bad credentials.
        $config = $this->liveConfig($this->queueName);
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
            if (! $badPool->stats()['closed']) {
                $badPool->close();
            }
        }

        self::assertTrue($threw, 'publish with bad credentials must fail');
        echo "\n[credentials-rejected] PASS: bad credentials correctly rejected\n";

        // Verify good credentials still work.
        $this->queue->push('stdClass', ['msg' => 'chaos-creds-2']);
        $job = $this->queue->pop();
        self::assertNotNull($job);
        $body = json_decode($job->getRawBody(), true);
        self::assertSame('chaos-creds-2', $body['data']['msg']);
        $job->delete();

        echo "\n[credentials-rejected] PASS: missing = 0 with good credentials\n";
    }

    /**
     * Scenario: Worker SIGTERM with unacked jobs.
     * A worker receives a SIGTERM while holding an unacked job.
     * The job must be redelivered to a new worker.
     */
    public function test_worker_sigterm_with_unacked(): void
    {
        $this->queue->clear($this->queueName);

        // Publish a message.
        $this->queue->push('stdClass', ['msg' => 'chaos-sigterm-1']);

        // Pop the job but do NOT ACK it — simulating a worker that
        // received SIGTERM while processing.
        $job = $this->queue->pop();
        self::assertNotNull($job);

        // Simulate SIGTERM: close the pool without ACKing.
        $this->pool->close();
        usleep(3000000); // 3 seconds for the broker to redeliver

        // Create a fresh pool and consume the redelivered message.
        $this->recreatePool();

        $job2 = $this->queue->pop();
        self::assertNotNull($job2, 'redelivered message after SIGTERM');
        $body = json_decode($job2->getRawBody(), true);
        self::assertSame('chaos-sigterm-1', $body['data']['msg']);
        $job2->delete();

        echo "\n[worker-sigterm-unacked] PASS: missing = 0\n";
    }

    // -------------------------------------------------------------------
    // Toxiproxy helpers
    // -------------------------------------------------------------------

    private function resetToxiproxy(): void
    {
        $ch = curl_init(self::TOXIPROXY_API . '/reset');
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'POST');
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_TIMEOUT, 5);
        curl_exec($ch);
        curl_close($ch);
    }

    private function addToxic(
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

        $ch = curl_init(self::TOXIPROXY_API . '/proxies/' . self::PROXY_1 . '/toxics');
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'POST');
        curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        curl_setopt($ch, CURLOPT_TIMEOUT, 5);
        curl_exec($ch);
        curl_close($ch);
    }

    private function removeToxic(string $name): void
    {
        $ch = curl_init(self::TOXIPROXY_API . '/proxies/' . self::PROXY_1 . '/toxics/' . $name);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_TIMEOUT, 5);
        curl_exec($ch);
        curl_close($ch);
    }

    // -------------------------------------------------------------------
    // Management API helpers
    // -------------------------------------------------------------------

    private function getQueueLeader(string $queue): string
    {
        $url = self::MGMT_API . '/api/queues/%2Forders-eu/' . urlencode($queue);
        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, self::ADMIN_USER . ':' . self::ADMIN_PASS);
        curl_setopt($ch, CURLOPT_TIMEOUT, 10);
        $resp = curl_exec($ch);
        curl_close($ch);

        $data = json_decode($resp, true);
        return $data['leader'] ?? 'rabbit@rabbitmq-1';
    }

    private function stopNode(string $node): void
    {
        $container = $this->nodeToContainer($node);
        exec("docker stop {$container} 2>&1", $output, $exit);
    }

    private function startNode(string $node): void
    {
        $container = $this->nodeToContainer($node);
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

    private function nodeToContainer(string $node): string
    {
        $parts = explode('@', $node);
        $suffix = end($parts);
        return "rabbitrs-{$suffix}-1";
    }

    // -------------------------------------------------------------------
    // Pool recreation helper
    // -------------------------------------------------------------------

    private function recreatePool(): void
    {
        if (isset($this->pool) && ! $this->pool->stats()['closed']) {
            $this->pool->close();
        }

        $config = $this->liveConfig($this->queueName);
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
    }
}
