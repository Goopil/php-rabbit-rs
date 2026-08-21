<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    $this->queueName = uniqueQueue();
    declareQueue($this->queueName);

    $config = liveConfig($this->queueName);
    $normalized = ConfigNormalizer::normalize($config);

    $this->pool = new Pool($normalized['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $this->pool);

    $connector = new RabbitMqConnector($factory, $normalized);
    $this->queue = $connector->connect([
        'queue' => $this->queueName,
        'block_for' => 3,
    ]);
    $this->queue->setContainer($this->app);
    $this->queue->setConnectionName('rabbit-rs-integration');
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

    expect($this->queue->size($this->queueName))->toBeGreaterThanOrEqual(2);

    $this->queue->clear($this->queueName);
    expect($this->queue->size($this->queueName))->toBe(0);
});
