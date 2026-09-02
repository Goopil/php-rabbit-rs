<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;

function pollForMessage(RabbitMqQueue $queue, int $timeoutSeconds): ?object
{
    $deadline = time() + $timeoutSeconds;
    while (time() < $deadline) {
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
        skip('ext-rabbit_rs is required for integration tests');
    }

    $this->queueName = uniqueQueue('rabbit-rs-it-delay');
    declareQueue($this->queueName);
    grantRabbitRsConfigure();

    // block_for must stay shorter than the delay used in the tests, otherwise
    // pop() blocks long enough to receive the delayed job itself and the
    // "not immediately available" assertion can never pass.
    [$this->pool, $this->queue] = integrationPoolAndQueue(
        $this->app,
        $this->queueName,
        connectOverrides: ['block_for' => 1],
    );
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
    $job = pollForMessage($this->queue, 5);
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
