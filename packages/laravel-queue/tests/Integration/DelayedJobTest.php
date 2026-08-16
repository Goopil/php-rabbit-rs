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
        $this->deleteQueue($this->queueName);
        parent::tearDown();
    }

    public function test_later_publishes_and_consumes_after_delay(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->later(2, 'stdClass', ['delayed' => 'job']);

        $job = $this->queue->pop();
        self::assertNull(
            $job,
            'a job published with a 2-second delay must not be immediately available',
        );

        // Wait for the delay to elapse, then poll for the job.
        usleep(2_500_000);
        $this->pollForMessage(5);

        $job = $this->queue->pop();
        self::assertNotNull($job, 'the delayed job should be available after the delay');
        $job->delete();
    }

    private function pollForMessage(int $timeoutSeconds): void
    {
        $deadline = time() + $timeoutSeconds;
        while (time() < $deadline) {
            $job = $this->queue->pop();
            if ($job !== null) {
                $job->delete();
                return;
            }
            usleep(200_000);
        }
    }

    public function test_later_with_zero_delay_behaves_like_push(): void
    {
        $this->queue->clear($this->queueName);

        $this->queue->later(0, 'stdClass', ['immediate' => 'job']);

        $job = $this->queue->pop();
        self::assertNotNull($job);
        $job->delete();
    }
}
