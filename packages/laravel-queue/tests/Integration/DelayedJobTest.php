<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

function pollForMessage(RabbitMqQueue $queue, int $timeoutSeconds): void
{
    $deadline = time() + $timeoutSeconds;
    while (time() < $deadline) {
        $job = $queue->pop();
        if ($job !== null) {
            $job->delete();
            return;
        }
        usleep(200_000);
    }
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-delay');
    declareQueue($this->queueName);

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
    $this->queue->setConnectionName('rabbit-rs-integration');
});

afterEach(function () {
    if (isset($this->pool) && ! $this->pool->stats()['closed']) {
        $this->pool->close();
    }
    deleteQueue($this->queueName);
});

it('publishes and consumes after delay', function () {
    $this->queue->clear($this->queueName);

    $this->queue->later(2, 'stdClass', ['delayed' => 'job']);

    $job = $this->queue->pop();
    $this->assertNull(
        $job,
        'a job published with a 2-second delay must not be immediately available',
    );

    // Wait for the delay to elapse, then poll for the job.
    usleep(2_500_000);
    pollForMessage($this->queue, 5);

    $job = $this->queue->pop();
    $this->assertNotNull($job, 'the delayed job should be available after the delay');
    $job->delete();
});

it('behaves like push with zero delay', function () {
    $this->queue->clear($this->queueName);

    $this->queue->later(0, 'stdClass', ['immediate' => 'job']);

    $job = $this->queue->pop();
    expect($job)->not->toBeNull();
    $job->delete();
});
