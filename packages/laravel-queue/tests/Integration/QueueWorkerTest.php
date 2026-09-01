<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    $this->queueName = uniqueQueue();
    declareQueue($this->queueName);

    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        connectOverrides: ['block_for' => 3],
    );
});

afterEach(function () {
    if (isset($this->pool) && ! $this->pool->stats()['closed']) {
        $this->pool->close();
    }
    deleteQueue($this->queueName);
});

it('pushes then pops and deletes', function () {
    $this->queue->clear($this->queueName);

    $this->queue->push('stdClass', ['message' => 'hello-integration']);

    $job = $this->queue->pop();
    expect($job)->not->toBeNull()
        ->toBeInstanceOf(RabbitMqJob::class);
    expect($job->getJobId())->not->toBeEmpty();

    $body = json_decode($job->getRawBody(), true);
    expect($body['job'])->toBe('stdClass')
        ->and($body['data'])->toBe(['message' => 'hello-integration']);

    $job->delete();
    expect($this->queue->pop())->toBeNull();
});

it('preserves raw payload when pushing raw', function () {
    $this->queue->clear($this->queueName);

    $payload = '{"custom":"raw-payload","uuid":"test-raw-1"}';
    $this->queue->pushRaw($payload, $this->queueName);

    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    expect($job->getRawBody())->toBe($payload);

    $job->delete();
});

it('bulk publishes then consumes all', function () {
    $this->queue->clear($this->queueName);

    $jobs = [];
    for ($i = 0; $i < 5; $i++) {
        $jobs[] = "stdClass:{$i}";
    }

    $this->queue->bulk($jobs, '', $this->queueName);

    $consumed = 0;
    for ($i = 0; $i < 5; $i++) {
        $job = $this->queue->pop();
        $this->assertNotNull($job, "expected job {$i}");
        $consumed++;
        $job->delete();
    }
    expect($consumed)->toBe(5);

    expect($this->queue->pop())->toBeNull();
});

it('requeues the job on release', function () {
    $this->queue->clear($this->queueName);

    $this->queue->push('stdClass', ['attempt' => 'release-test']);

    $job = $this->queue->pop();
    expect($job)->not->toBeNull();

    $job->release(0);

    $requeued = $this->queue->pop();
    expect($requeued)->not->toBeNull();
    $requeued->delete();
});

it('returns size zero after clear', function () {
    $this->queue->clear($this->queueName);
    expect($this->queue->size($this->queueName))->toBe(0);
});

it('increases size after push', function () {
    $this->queue->clear($this->queueName);

    $this->queue->push('stdClass', ['size' => 'test']);
    $this->queue->push('stdClass', ['size' => 'test2']);

    // Publishes are batched in the native publish buffer and only reach the
    // broker on the next flush (threshold, consumer drain, or explicit
    // flush()); size() reads broker state, so flush before asserting.
    $this->pool->flush();

    expect($this->queue->size($this->queueName))->toBeGreaterThanOrEqual(2);

    $this->queue->clear($this->queueName);
    expect($this->queue->size($this->queueName))->toBe(0);
});
