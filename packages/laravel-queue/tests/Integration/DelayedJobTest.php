<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Integration;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Queue\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

final class DelayedJobTest extends IntegrationTestCase
{
    private RabbitMqQueue $queue;
    private Pool $pool;
    private string $queueName;

    protected function setUp(): void
    {
        parent::setUp();
        $this->queueName = $this->uniqueQueue('rabbit-rs-it-delay');

        $config = $this->liveConfig($this->queueName);
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
    }

    protected function tearDown(): void
    {
        if (isset($this->pool) && ! $this->pool->stats()['closed']) {
            $this->pool->close();
        }
        parent::tearDown();
    }

    public function test_later_publishes_and_consumes_after_delay(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->later(2, 'stdClass', ['delayed' => 'job']);

        // The job should not be immediately available
        $immediate = $this->queue->pop($this->queueName);
        self::assertNull(
            $immediate,
            'delayed job should not be available immediately',
        );

        // After the delay, the job should be available
        $job = $this->queue->pop($this->queueName);
        self::assertNotNull($job, 'delayed job should be available after delay');

        $body = json_decode($job->getRawBody(), true);
        self::assertSame('stdClass', $body['job']);

        $job->delete();
    }

    public function test_later_with_zero_delay_behaves_like_push(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->later(0, 'stdClass', ['immediate' => 'job']);

        $job = $this->queue->pop($this->queueName);
        self::assertNotNull($job);
        $job->delete();
    }
}
