<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Integration;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
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
        $this->declareQueue($this->queueName);

        $config = $this->liveConfig($this->queueName);
        $normalized = ConfigNormalizer::normalize($config);

        $this->pool = new Pool($normalized['native']);
        $factory = new NativePoolFactory(createPool: fn (): Pool => $this->pool);

        $connector = new RabbitMqConnector($factory, $normalized);
        $this->queue = $connector->connect([
            'queue' => 'default',
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
        $this->deleteQueue($this->queueName);
        parent::tearDown();
    }

    /**
     * Publisher-side delay routing (DelayRouter → x-delayed-message exchange)
     * is not yet wired into the publish path. The DelayRouter infrastructure
     * exists but is only used by the consumer's release() method.
     *
     * @see docs/plans/2026-07-30-rabbitmq-native-implementation.md Task 26
     */
    public function test_later_publishes_and_consumes_after_delay(): void
    {
        self::markTestSkipped('Publisher-side delay routing not yet implemented');
    }

    public function test_later_with_zero_delay_behaves_like_push(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->later(0, 'stdClass', ['immediate' => 'job'], $this->queueName);

        $job = $this->queue->pop();
        self::assertNotNull($job);
        $job->delete();
    }
}
