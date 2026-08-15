<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Integration;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\Queue\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

final class QueueWorkerTest extends IntegrationTestCase
{
    private RabbitMqQueue $queue;
    private Pool $pool;
    private string $queueName;

    protected function setUp(): void
    {
        parent::setUp();
        $this->queueName = $this->uniqueQueue();

        $config = $this->liveConfig($this->queueName);
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
    }

    protected function tearDown(): void
    {
        if (isset($this->pool) && ! $this->pool->stats()['closed']) {
            $this->pool->close();
        }
        parent::tearDown();
    }

    public function test_push_then_pop_and_delete(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->push('stdClass', ['message' => 'hello-integration']);

        $job = $this->queue->pop($this->queueName);
        self::assertNotNull($job);
        self::assertInstanceOf(RabbitMqJob::class, $job);
        self::assertNotEmpty($job->getJobId());

        $body = json_decode($job->getRawBody(), true);
        self::assertSame('stdClass', $body['job']);
        self::assertSame(['message' => 'hello-integration'], $body['data']);

        $job->delete();
        self::assertNull($this->queue->pop($this->queueName));
    }

    public function test_push_raw_preserves_payload(): void
    {
        $this->queue->clear($this->queueName);

        $payload = '{"custom":"raw-payload","uuid":"test-raw-1"}';
        $this->queue->pushRaw($payload, $this->queueName);

        $job = $this->queue->pop($this->queueName);
        self::assertNotNull($job);
        self::assertSame($payload, $job->getRawBody());

        $job->delete();
    }

    public function test_bulk_publish_then_consume_all(): void
    {
        $this->queue->clear($this->queueName);

        $jobs = [];
        for ($i = 0; $i < 5; $i++) {
            $jobs[] = "stdClass:{$i}";
        }

        $this->queue->bulk($jobs, '', $this->queueName);

        $consumed = 0;
        for ($i = 0; $i < 5; $i++) {
            $job = $this->queue->pop($this->queueName);
            self::assertNotNull($job, "expected job {$i}");
            $consumed++;
            $job->delete();
        }
        self::assertSame(5, $consumed);

        self::assertNull($this->queue->pop($this->queueName));
    }

    public function test_release_requeues_the_job(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->push('stdClass', ['attempt' => 'release-test']);

        $job = $this->queue->pop($this->queueName);
        self::assertNotNull($job);

        $job->release(0);

        $requeued = $this->queue->pop($this->queueName);
        self::assertNotNull($requeued);
        $requeued->delete();
    }

    public function test_size_returns_zero_after_clear(): void
    {
        $this->queue->clear($this->queueName);
        self::assertSame(0, $this->queue->size($this->queueName));
    }

    public function test_size_increases_after_push(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->push('stdClass', ['size' => 'test']);
        $this->queue->push('stdClass', ['size' => 'test2']);

        self::assertGreaterThanOrEqual(2, $this->queue->size($this->queueName));

        $this->queue->clear($this->queueName);
        self::assertSame(0, $this->queue->size($this->queueName));
    }
}
