<?php

declare(strict_types=1);

/*
 * Characterization for the native consumer drain-ahead (issue #260):
 * the framework pop() loop pays one blocking handoff per delivery today,
 * while the flume buffer already holds the sibling deliveries. The
 * drain-ahead must hand those siblings out in FIFO push order with every
 * returned job individually ackable, and the pipeline must read empty once
 * drained. These invariants must hold before and after the amortization.
 */

it('delivers buffered siblings in FIFO order with per-job acks', function () {    $this->queueName = uniqueQueue('rabbit-rs-it-drain');
    declareQueue($this->queueName);
    grantRabbitRsConfigure(ORDERS_VHOST);

    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        connectOverrides: ['block_for' => 10],
    );

    for ($i = 0; $i < 5; $i++) {
        $this->queue->push('stdClass', ['seq' => $i]);
    }

    for ($i = 0; $i < 5; $i++) {
        $job = $this->queue->pop();
        expect($job)->not->toBeNull("pop #{$i} must return a job");
        expect(json_decode($job->getRawBody(), true)['data']['seq'])
            ->toBe($i, 'FIFO push order must be preserved across buffered siblings');
        $job->delete();
    }

    expect($this->queue->pop())->toBeNull('the pipeline must be fully drained after the batch');
});
