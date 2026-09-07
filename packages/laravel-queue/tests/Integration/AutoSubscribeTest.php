<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;

/**
 * Whether the broker knows the queue: the management API answers with the
 * queue's payload when it exists and an "Object Not Found" error otherwise.
 */
function queueExistsOnBroker(string $queueName): bool
{
    $data = json_decode(managementRequest(
        'GET',
        'http://localhost:15672/api/queues/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($queueName),
    ), true);

    return is_array($data) && ($data['name'] ?? null) === $queueName;
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    grantRabbitRsConfigure();

    // The connection's own queue carries the configured worker profile; the
    // target queue appears in no workers.* entry and is never pre-declared:
    // only the synthesized __auto__. profile can cover it.
    $source = uniqueQueue('rabbit-rs-it-auto-main');
    $this->target = uniqueQueue('rabbit-rs-it-auto');
    $this->cleanupQueues = [$source, $this->target];

    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $source,
        ['auto_subscribe' => true],
        ['block_for' => 3],
    );
});

afterEach(function () {
    if (isset($this->pool) && ! $this->pool->stats()['closed']) {
        $this->pool->close();
    }
    if (isset($this->cleanupQueues)) {
        foreach ($this->cleanupQueues as $queue) {
            deleteQueue($queue);
        }
    }
});

it('pops, marshals and acks a job published to an undeclared queue', function () {
    // First pop synthesizes the __auto__. profile, declares the queue and
    // starts the consumer; nothing is published yet, so it times out empty.
    // The queue must exist before the push: mandatory publishing treats a
    // missing queue as unroutable, so pushing first would fail the publish.
    expect($this->queue->pop($this->target))->toBeNull();

    $this->queue->push('stdClass', ['auto' => 'subscribe'], $this->target);

    $job = $this->queue->pop($this->target);

    expect($job)->toBeInstanceOf(RabbitMqJob::class)
        ->and($job->getQueue())->toBe($this->target);

    $job->delete();

    expect($this->queue->size($this->target))->toBe(0)
        ->and(queueExistsOnBroker($this->target))->toBeTrue();
});
