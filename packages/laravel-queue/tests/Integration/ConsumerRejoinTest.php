<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Pool;
use Goopil\RabbitRs\Laravel\Exceptions\QueueException;

const REJOIN_MGMT_API = 'http://localhost:15672';

/**
 * Declares the test queue in the /billing vhost. This test runs in /billing
 * so the connection-kill chaos below can never disturb other suites running
 * against /orders-eu in the shared lab.
 */
function rejoinDeclareQueue(string $queueName): void
{
    $url = REJOIN_MGMT_API.'/api/queues/%2Fbilling/'.urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
    curl_setopt($ch, CURLOPT_POSTFIELDS, json_encode([
        'durable' => true,
        'arguments' => ['x-queue-type' => 'quorum'],
    ]));
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_exec($ch);
}

function rejoinDeleteQueue(string $queueName): void
{
    $url = REJOIN_MGMT_API.'/api/queues/%2Fbilling/'.urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_exec($ch);
}

/**
 * Kills every AMQP connection to the /billing vhost — this test's pool is
 * its only tenant there — forcing it through a recovery generation bump:
 * the coordinator reconnects and recover_generation replaces the profile's
 * consumer set, closing the one a previously fetched consumer holds.
 *
 * Returns the number of connections killed; the test asserts on it so the
 * chaos injection can never silently turn into a no-op.
 */
function killBillingConnections(): int
{
    $ch = curl_init(REJOIN_MGMT_API.'/api/connections');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_setopt($ch, CURLOPT_TIMEOUT, 10);
    $response = curl_exec($ch);

    $killed = 0;
    foreach (json_decode((string) $response, true) ?: [] as $connection) {
        if (($connection['vhost'] ?? null) !== '/billing') {
            continue;
        }
        $ch = curl_init(REJOIN_MGMT_API.'/api/connections/'.rawurlencode((string) $connection['name']));
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
        curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['X-Reason: consumer-rejoin-test']);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_TIMEOUT, 10);
        curl_exec($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        if ($status < 200 || $status >= 300) {
            fwrite(STDERR, "\nDEBUG kill $connection[name] -> HTTP $status\n");
            continue;
        }
        $killed++;
    }

    return $killed;
}

/**
 * Closes the pool best-effort: a pool whose connection was bounced by the
 * chaos kill can throw from close() even though the handle is released.
 */
function rejoinClosePool(?Pool $pool): void
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
        skip('ext-rabbit_rs is required for integration tests');
    }

    // The lazy native publisher declares rabbit-rs.* internal exchanges; the
    // lab's stored /billing configure permission only allows amq.* and
    // rabbit-rs-it-* names, so extend it like the /orders-eu tests do.
    grantRabbitRsConfigure('/billing');
});

afterEach(function () {
    rejoinClosePool(isset($this->pool) ? $this->pool : null);
    if (isset($this->queueName)) {
        rejoinDeleteQueue($this->queueName);
    }
});

/*
 * A recovered broker must rejoin a long-lived worker: the queue's consumer
 * cache holds a handle whose broker source was replaced, and the native side
 * surfaces the retire (SourceReplaced / Closed) only through the cached
 * handle. The queue must evict the entry on those errors so the next pop
 * re-fetches a fresh consumer and deliveries resume.
 */
it('rejoins a recovered broker after the cached consumer retires', function () {
    $this->queueName = uniqueQueue('rabbit-rs-it-rejoin');
    rejoinDeclareQueue($this->queueName);

    $config = liveConfig($this->queueName);
    $config['brokers']['default']['vhost'] = '/billing';
    $config['brokers'] = ['rejoin-broker' => $config['brokers']['default']];
    $config['routes'] = ['default' => ['broker' => 'rejoin-broker', 'exchange' => '', 'routing_key' => '{queue}']];
    $config['workers']['default']['subscriptions']['default']['broker'] = 'rejoin-broker';

    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        $config,
        connectOverrides: ['block_for' => 10],
        connectionName: 'rabbit-rs-rejoin',
    );

    // Warm-up pop: caches the consumer for the first connection generation.
    expect($this->queue->pop())->toBeNull();
    $reconnectsBefore = $this->pool->stats()['reconnects_total'] ?? 0;

    // Force a native generation bump: kill the pool's AMQP connection.
    expect(killBillingConnections())->toBe(1);

    // Wait for the recovery generation to complete before publishing.
    $deadline = microtime(true) + 30;
    do {
        usleep(200000);
        $reconnects = $this->pool->stats()['reconnects_total'] ?? 0;
        expect(microtime(true))->toBeLessThan($deadline + 5, 'the pool never recovered from the connection kill');
    } while ($reconnects <= $reconnectsBefore);

    $this->queue->push('stdClass', ['msg' => 'rejoin-1']);

    // The first pops surface the retired handle's error (Closed or the
    // one-shot re-fetch signal): the queue must evict and re-fetch instead
    // of replaying the retired handle's error forever.
    $deadline = microtime(true) + 30;
    $msg = null;
    while (microtime(true) < $deadline) {
        try {
            $job = $this->queue->pop();
            if ($job !== null) {
                $msg = json_decode($job->getRawBody(), true)['data']['msg'] ?? '';
                $job->delete();
                break;
            }
        } catch (QueueException | ConnectionException) {
            // Expected while the retired handle's errors surface; a hot loop
            // here means the cache was never evicted.
        }
        usleep(100000);
    }

    expect($msg)->toBe('rejoin-1', 'the queue must re-fetch the consumer and deliver after recovery');
});
