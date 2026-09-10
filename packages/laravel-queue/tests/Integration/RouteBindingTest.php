<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    // Issue #205: nothing is pre-declared here. The declare-mode pool must
    // provision the queue itself, the publish route exchange, and the route
    // binding — otherwise the push below is unroutable and the pop stays
    // empty. The lab's stored configure permission only allows amq.* and
    // rabbit-rs-it-* names, so the unique exchange lives inside that regex.
    grantRabbitRsConfigure();

    $this->queueName = uniqueQueue();
    $this->exchangeName = uniqueQueue();

    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        configOverrides: [
            'exchange' => $this->exchangeName,
            'routing_key' => '{queue}',
        ],
        connectOverrides: ['block_for' => 3],
    );
});

afterEach(function () {
    if (isset($this->pool) && ! $this->pool->stats()['closed']) {
        $this->pool->close();
    }
    deleteQueue($this->queueName);
    // PoisonDeliveryTest declares a local deleteExchange(); keep the helper
    // local here too so the two never collide in one Pest run.
    managementRequest('DELETE', 'http://localhost:15672/api/exchanges/'.rawurlencode(ORDERS_VHOST).'/'.urlencode($this->exchangeName));
});

it('declares the publish route so a freshly declared queue is reachable', function () {
    $this->queue->push('stdClass', ['message' => 'route-binding']);

    $job = $this->queue->pop();
    expect($job)->not->toBeNull()
        ->toBeInstanceOf(RabbitMqJob::class);

    $body = json_decode($job->getRawBody(), true);
    expect($body['data'])->toBe(['message' => 'route-binding']);

    $job->delete();
    expect($this->queue->pop())->toBeNull();
});
