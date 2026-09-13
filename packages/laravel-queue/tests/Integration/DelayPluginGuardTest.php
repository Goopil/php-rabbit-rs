<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Exceptions\DelayPluginMissingException;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\DelayPluginGuard;

/**
 * The lab's delayed-message plugin state, read from the management API the
 * same way the guard reads it. The lab ships with-plugin and without-plugin
 * profiles on the same ports, so the suite discovers the state instead of
 * assuming it and pins the matching behavior per mode.
 */
function delayGuardBrokerHasPlugin(): bool
{
    $overview = json_decode(managementRequest('GET', 'http://localhost:15672/api/overview'), true);
    $types = is_array($overview) ? ($overview['exchange_types'] ?? []) : [];

    foreach ($types as $type) {
        if (is_array($type) && ($type['name'] ?? null) === 'x-delayed-message') {
            return true;
        }
    }

    return false;
}

/**
 * Number of ttl bucket queues on the vhost. Buckets carry the
 * rabbit-rs.delay. prefix and self-expire after their idle window.
 */
function delayGuardBucketCount(): int
{
    $queues = json_decode(managementRequest('GET', 'http://localhost:15672/api/queues/'.rawurlencode(ORDERS_VHOST)), true);

    $count = 0;
    foreach (is_array($queues) ? $queues : [] as $queue) {
        if (is_array($queue) && is_string($queue['name'] ?? null) && str_starts_with($queue['name'], 'rabbit-rs.delay.')) {
            $count++;
        }
    }

    return $count;
}

/**
 * Resolves a rabbit-rs connection through the queue manager (the connector
 * path the delay guard rides), returning the queue. The connection carries
 * the lab's management URL so the guard's probe hits the real broker.
 */
function delayGuardQueue(string $connectionName, string $queueName, string $mode): object
{
    config()->set('queue.connections.'.$connectionName, array_merge(liveConfig($queueName), [
        'management_url' => 'http://localhost:15672',
        'block_for' => 1,
        'delay' => ['mode' => $mode],
    ]));

    return app('queue')->connection($connectionName);
}

function delayGuardPoll(RabbitMqQueue $queue, float $timeoutSeconds): ?object
{
    $deadline = microtime(true) + $timeoutSeconds;
    while (microtime(true) < $deadline) {
        $job = $queue->pop();
        if ($job !== null) {
            return $job;
        }
        usleep(200_000);
    }

    return null;
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        $this->markTestSkipped('ext-rabbit_rs is required for integration tests');
    }

    $this->brokerHasPlugin = delayGuardBrokerHasPlugin();
    DelayPluginGuard::reset();
});

afterEach(function () {
    DelayPluginGuard::reset();
    if (isset($this->pool)) {
        $pool = $this->pool;
        if (is_object($pool) && ! $pool->stats()['closed']) {
            $pool->close();
        }
    }
});

it('auto mode routes delayed jobs into ttl buckets on a plugin-less broker', function () {
    if ($this->brokerHasPlugin) {
        $this->markTestSkipped('the lab broker has the delayed-message plugin: auto keeps the plugin strategy');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-delay-auto');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    $bucketsBefore = delayGuardBucketCount();
    $queue = delayGuardQueue('rabbit-rs-it-delay-auto', $this->queueName, 'auto');
    $this->pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);
    $queue->clear($this->queueName);

    // Bug 15's live evidence: the broken auto path publishes into the main
    // queue and creates no bucket. The fix routes into a ttl bucket queue
    // like explicit ttl mode does — bucket declaration may happen at pool
    // topology reconciliation or at the first delayed publish, so the
    // count is taken against the pre-resolution baseline.
    $queue->later(2, 'stdClass', ['delayed' => 'job']);

    expect(delayGuardBucketCount())->toBeGreaterThan($bucketsBefore);

    // Bucket release is lazily swept by the broker (documented ceiling),
    // so the arrival poll uses the same generous window as the ttl suite.
    $job = delayGuardPoll($queue, 45);
    expect($job)->not->toBeNull('the delayed job should arrive after its delay');
    $job->delete();
});

it('auto mode resolves the ttl strategy against the plugin-less broker', function () {
    if ($this->brokerHasPlugin) {
        $this->markTestSkipped('the lab broker has the delayed-message plugin: auto keeps the plugin strategy');
    }

    config()->set('queue.connections.rabbit-rs-it-delay-probe', array_merge(liveConfig('orders'), [
        'management_url' => 'http://localhost:15672',
    ]));

    expect(DelayPluginGuard::resolveAutoMode('rabbit-rs-it-delay-probe', 'auto'))->toBe('ttl');
});

it('auto mode resolves the plugin strategy against a plugin-enabled broker', function () {
    if (! $this->brokerHasPlugin) {
        $this->markTestSkipped('the lab broker has no delayed-message plugin: auto degrades to ttl');
    }

    config()->set('queue.connections.rabbit-rs-it-delay-probe', array_merge(liveConfig('orders'), [
        'management_url' => 'http://localhost:15672',
    ]));

    expect(DelayPluginGuard::resolveAutoMode('rabbit-rs-it-delay-probe', 'auto'))->toBe('auto');
});

it('plugin mode refuses the delayed publish when the plugin is absent', function () {
    if ($this->brokerHasPlugin) {
        $this->markTestSkipped('the lab broker has the delayed-message plugin: plugin mode publishes');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-delay-plugin');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    $queue = delayGuardQueue('rabbit-rs-it-delay-plugin', $this->queueName, 'plugin');
    $this->pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);

    try {
        $queue->later(2, 'stdClass', ['delayed' => 'job']);
        $this->fail('expected DelayPluginMissingException');
    } catch (DelayPluginMissingException) {
    }

    expect($queue->size($this->queueName))->toBe(0);
});

it('plugin mode publishes when the plugin is present', function () {
    if (! $this->brokerHasPlugin) {
        $this->markTestSkipped('the lab broker has no delayed-message plugin: plugin mode refuses');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-delay-plugin');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    $queue = delayGuardQueue('rabbit-rs-it-delay-plugin', $this->queueName, 'plugin');
    $this->pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);
    $queue->clear($this->queueName);

    $queue->later(2, 'stdClass', ['delayed' => 'job']);

    $job = delayGuardPoll($queue, 45);
    expect($job)->not->toBeNull('the delayed job should arrive after its delay');
    $job->delete();
});

it('plugin mode publishes through when the management api is unreachable', function () {
    $this->queueName = uniqueQueue('rabbit-rs-it-delay-failopen');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    config()->set('queue.connections.rabbit-rs-it-delay-failopen', array_merge(liveConfig($this->queueName), [
        'management_url' => 'http://localhost:59999',
        'block_for' => 1,
        'delay' => ['mode' => 'plugin'],
    ]));
    $queue = app('queue')->connection('rabbit-rs-it-delay-failopen');
    $this->pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue);
    $queue->clear($this->queueName);

    // Unverifiable plugin state must not break a working plugin setup:
    // the publish goes through unchanged (logged once), exactly the
    // pre-guard behavior for this case.
    $queue->later(2, 'stdClass', ['delayed' => 'job']);

    $job = delayGuardPoll($queue, 45);
    expect($job)->not->toBeNull('the delayed job should still be delivered');
    $job->delete();
});
