<?php

declare(strict_types=1);

use Goopil\RabbitRs\Pool;

/** Second lab vhost the two-connection tests operate on. */
const BILLING_VHOST = '/billing';

/**
 * Closes a pool best-effort. A pool that hit chaos-free errors (e.g. closed
 * while a consumer is mid-pop) can still throw from close(); cleanup must
 * never mask the test outcome. Named locally so the whole-suite run never
 * collides with the other suites' helpers.
 */
function twoClosePool(?Pool $pool): void
{
    if ($pool === null) {
        return;
    }
    try {
        if (! $pool->stats()['closed']) {
            $pool->close();
        }
    } catch (\Throwable) {
        // best-effort cleanup
    }
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    // Declare-mode pools provision rabbit-rs.* internal exchanges; the lab's
    // stored configure permission only allows amq.* and rabbit-rs-it-* names.
    grantRabbitRsConfigure();
    grantRabbitRsConfigure(BILLING_VHOST);
});

afterEach(function () {
    twoClosePool(isset($this->poolA) ? $this->poolA : null);
    twoClosePool(isset($this->poolB) ? $this->poolB : null);
    if (isset($this->queueA)) {
        deleteQueue($this->queueA);
    }
    if (isset($this->queueB)) {
        deleteQueue($this->queueB, BILLING_VHOST);
    }
});

/*
 * Two rabbit-rs connections over two distinct brokers/vhosts of the lab
 * cluster (rabbitmq-1:/orders-eu and rabbitmq-2:/billing) — one vhost owns
 * one AMQP connection. A job dispatched on A is consumed by A only; a job
 * dispatched on B is consumed by B only; neither connection ever sees the
 * other's messages.
 */
it('keeps two connections isolated across brokers and vhosts', function () {
    $this->queueA = uniqueQueue('rabbit-rs-it-two-a');
    $this->queueB = uniqueQueue('rabbit-rs-it-two-b');
    declareQueue($this->queueA);
    declareQueue($this->queueB, BILLING_VHOST);

    [$this->poolA, $queueA] = integrationPoolAndQueue(
        $this->app,
        $this->queueA,
        connectOverrides: ['block_for' => 3],
        connectionName: 'rabbit-rs-two-a',
    );
    [$this->poolB, $queueB] = integrationPoolAndQueue(
        $this->app,
        $this->queueB,
        configOverrides: ['hosts' => '127.0.0.1:5673', 'vhost' => BILLING_VHOST],
        connectOverrides: ['block_for' => 3],
        connectionName: 'rabbit-rs-two-b',
    );

    // Warm-up pops attach both connections' consumers.
    expect($queueA->pop())->toBeNull()
        ->and($queueB->pop())->toBeNull();

    // Dispatch on A: A delivers it, B never sees it.
    $queueA->push('stdClass', ['msg' => 'only-on-a']);

    $job = $queueA->pop();
    expect($job)->not->toBeNull()
        ->and(json_decode($job->getRawBody(), true)['data']['msg'] ?? '')->toBe('only-on-a');
    $job->delete();

    expect($queueB->size($this->queueB))->toBe(0)
        ->and($queueB->pop())->toBeNull();

    // Dispatch on B: B delivers it, A never sees it.
    $queueB->push('stdClass', ['msg' => 'only-on-b']);

    $job = $queueB->pop();
    expect($job)->not->toBeNull()
        ->and(json_decode($job->getRawBody(), true)['data']['msg'] ?? '')->toBe('only-on-b');
    $job->delete();

    expect($queueA->size($this->queueA))->toBe(0)
        ->and($queueA->pop())->toBeNull();
});

/*
 * Two queue:work-style consumers (two pools, what two worker processes on one
 * queue.connections entry produce) share one queue. Both attach their own
 * consumer with its own channel-scoped tag — no collision — and the workload
 * is delivered exactly once across them.
 */
it('runs two consumers on one queue without tag collision', function () {
    $this->queueA = uniqueQueue('rabbit-rs-it-two-shared');
    declareQueue($this->queueA);

    // Both workers run the same queue but land on distinct AMQP connections:
    // the process-local runtime registry dedupes identical fingerprints, so
    // the second worker's config routes through rabbitmq-2 — what two
    // queue:work processes on different cluster nodes produce.
    [$poolOne, $consumerOne] = integrationPoolAndQueue(
        $this->app,
        $this->queueA,
        connectOverrides: ['block_for' => 3],
        connectionName: 'rabbit-rs-two-shared',
    );
    [$poolTwo, $consumerTwo] = integrationPoolAndQueue(
        $this->app,
        $this->queueA,
        configOverrides: ['hosts' => '127.0.0.1:5673'],
        connectOverrides: ['block_for' => 3],
        connectionName: 'rabbit-rs-two-shared-2',
    );
    $this->poolA = $poolOne;
    $this->poolB = $poolTwo;

    // Warm-up pops attach both consumers before the workload exists.
    expect($consumerOne->pop())->toBeNull()
        ->and($consumerTwo->pop())->toBeNull();

    $expected = [];
    for ($i = 0; $i < 6; $i++) {
        $expected[] = "shared-{$i}";
        $consumerOne->push('stdClass', ['msg' => "shared-{$i}"]);
    }

    // Consume from both workers until the workload is drained.
    $received = [];
    $deadline = microtime(true) + 30;
    while (count($received) < 6 && microtime(true) < $deadline) {
        foreach ([$consumerOne, $consumerTwo] as $queue) {
            $job = $queue->pop();
            if ($job !== null) {
                $received[] = json_decode($job->getRawBody(), true)['data']['msg'] ?? '';
                $job->delete();
            }
        }
    }

    // Every message delivered, none duplicated, and no consumer-tag
    // collision: a duplicate tag errors the channel, pops would throw and
    // the split below would collapse onto one consumer.
    expect($received)->toHaveCount(6)
        ->and(count(array_unique($received)))->toBe(6)
        ->and(array_diff($expected, $received))->toBe([]);

    // Both connections actually carried traffic: the broker round-robins
    // deliveries across the two consumers rather than one starving the
    // other (the lab's management API has stats disabled, so the native
    // counters are the observable here).
    $oneDelivered = $poolOne->stats()['deliveries_total'] ?? 0;
    $twoDelivered = $poolTwo->stats()['deliveries_total'] ?? 0;
    expect($oneDelivered + $twoDelivered)->toBe(6)
        ->and($oneDelivered)->toBeGreaterThan(0)
        ->and($twoDelivered)->toBeGreaterThan(0);
});
