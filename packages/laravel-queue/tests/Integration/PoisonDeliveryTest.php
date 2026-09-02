<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;
use Illuminate\Queue\Events\JobFailed;
use Illuminate\Queue\Failed\NullFailedJobProvider;
use Illuminate\Queue\WorkerOptions;
use Illuminate\Support\Facades\Log;

class IntegrationPoisonFailingJob
{
    public function fire($job, $data): void
    {
        throw new RuntimeException('rabbit-rs integration: this job always fails');
    }
}

/**
 * Declares the source queue with the same arguments the pool's declare-mode
 * reconcile sends (quorum, durable, dead-letter arguments). Declaring up front
 * keeps the worker's basic.consume from racing the quorum queue leader
 * election on a fresh queue.
 */
function queueMessageCount(string $queueName): int
{
    $url = 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    $response = curl_exec($ch);
    curl_close($ch);

    $data = json_decode((string) $response, true);

    return (int) ($data['messages'] ?? 0);
}

function deleteExchange(string $exchangeName): void
{
    $url = 'http://localhost:15672/api/exchanges/%2Forders-eu/'.urlencode($exchangeName);
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_exec($ch);
    curl_close($ch);
}

/**
 * Declares the source queue with the same arguments the pool's declare-mode
 * reconcile sends (quorum, durable, dead-letter arguments). Declaring up front
 * keeps the worker's basic.consume from racing the quorum queue leader
 * election on a fresh queue.
 */
function declareSourceQueueWithDeadLetter(string $queueName, string $dlx, string $routingKey): void
{
    $url = 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName);
    $payload = json_encode([
        'durable' => true,
        'arguments' => [
            'x-queue-type' => 'quorum',
            'x-dead-letter-exchange' => $dlx,
            'x-dead-letter-routing-key' => $routingKey,
        ],
    ]);

    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
    curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    curl_exec($ch);
    curl_close($ch);
}

/**
 * Wires the shared pool and connector used by every poison test. The pool is
 * stored on the test case so afterEach can close it.
 */
function connectPoisonQueue($test, array $normalized, string $source): RabbitMqQueue
{
    $test->pool = new Pool($normalized['native']);
    $connector = new RabbitMqConnector(
        new NativePoolFactory(createPool: fn (): Pool => $test->pool),
        $normalized,
    );
    $test->connector = $connector;

    return $connector->connect(['queue' => $source, 'block_for' => 3]);
}

beforeEach(function () {
    if (! extension_loaded('rabbit_rs')) {
        skip('ext-rabbit_rs is required for integration tests');
    }

    // Declare-mode pools with the default auto delay strategy now provision
    // the rabbit-rs.delayed exchange themselves (issue #97); the lab's stored
    // configure permission only allows amq.* and rabbit-rs-it-* names.
    grantRabbitRsConfigure();
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
    if (isset($this->cleanupExchanges)) {
        foreach ($this->cleanupExchanges as $exchange) {
            deleteExchange($exchange);
        }
    }
});

it('dead-letters an unmarshable delivery when a dead-letter exchange is configured', function () {
    $source = uniqueQueue('rabbit-rs-it-poison-dlx');
    $dlq = uniqueQueue('rabbit-rs-it-poison-dlq');
    $dlx = uniqueQueue('rabbit-rs-it-poison-dlx-ex');
    $this->cleanupQueues = [$source, $dlq];
    $this->cleanupExchanges = [$dlx];

    $config = liveConfig($source);
    $config['topology']['dead_letter'] = ['exchange' => $dlx, 'queue' => $dlq, 'routing_key' => null];
    $normalized = ConfigNormalizer::normalize($config);

    // The declare-mode reconcile defaults the dead-letter routing key to the
    // source queue name; pre-declare with identical arguments so the worker's
    // basic.consume cannot race the quorum queue leader election.
    declareSourceQueueWithDeadLetter($source, $dlx, $source);

    $this->queue = connectPoisonQueue($this, $normalized, $source);
    $this->queue->setContainer($this->app);
    $this->queue->setConnectionName('rabbit-rs-poison');

    // Warm-up pop: waits for the generation that declares the source queue
    // with its dead-letter arguments, the DLX, the DLQ and the binding.
    expect($this->queue->pop())->toBeNull();

    Log::spy();

    $this->queue->pushRaw('this is not json at all', $source);

    // The unmarshable delivery must be settled terminally, not returned.
    expect($this->queue->pop())->toBeNull();
    expect($this->queue->size($source))->toBe(0);
    expect($this->queue->size($dlq))->toBe(1);
    expect($this->queue->pop())->toBeNull();

    Log::shouldHaveReceived('error');
});

it('acknowledges an unmarshable delivery when no dead-letter exchange is configured', function () {
    $source = uniqueQueue('rabbit-rs-it-poison-ack');
    $this->cleanupQueues = [$source];

    $normalized = ConfigNormalizer::normalize(liveConfig($source));

    $this->queue = connectPoisonQueue($this, $normalized, $source);
    $this->queue->setContainer($this->app);
    $this->queue->setConnectionName('rabbit-rs-poison');

    expect($this->queue->pop())->toBeNull();

    Log::spy();

    $this->queue->pushRaw('still not json', $source);

    expect($this->queue->pop())->toBeNull();
    expect($this->queue->size($source))->toBe(0);
    expect($this->queue->pop())->toBeNull();

    Log::shouldHaveReceived('error');
});

it('fails a job that always throws when the maximum number of tries is reached', function () {
    $source = uniqueQueue('rabbit-rs-it-poison-worker');
    $this->cleanupQueues = [$source];

    $normalized = ConfigNormalizer::normalize(liveConfig($source));

    $this->queue = connectPoisonQueue($this, $normalized, $source);
    $this->queue->setContainer($this->app);
    $this->queue->setConnectionName('rabbit-rs-integration');

    $this->app->instance('queue.failer', new NullFailedJobProvider);
    $this->app['config']->set('queue.connections.rabbit-rs-worker-it', [
        'driver' => 'rabbit-rs',
        'queue' => $source,
        'block_for' => 3,
    ]);
    $this->app['queue']->extend('rabbit-rs', fn (): RabbitMqConnector => $this->connector);

    $failures = [];
    $this->app['events']->listen(JobFailed::class, function (JobFailed $event) use (&$failures): void {
        $failures[] = $event;
    });

    $this->queue->push(IntegrationPoisonFailingJob::class, ['test' => 'tries']);

    $worker = $this->app->make('queue.worker');
    // First run: the job fires once, throws, attempts (1) < maxTries (2) → release.
    $worker->runNextJob('rabbit-rs-worker-it', $source, new WorkerOptions(maxTries: 2));
    // Second run: the redelivered job reports attempts (2) >= maxTries (2) → fail.
    $worker->runNextJob('rabbit-rs-worker-it', $source, new WorkerOptions(maxTries: 2));

    expect($failures)->toHaveCount(1)
        ->and($failures[0]->job->attempts())->toBe(2)
        ->and($this->queue->size($source))->toBe(0)
        ->and($this->queue->pop())->toBeNull();
});
