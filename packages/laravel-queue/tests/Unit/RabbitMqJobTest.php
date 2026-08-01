<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Closure;
use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Events\Dispatcher;
use Illuminate\Queue\Events\JobFailed;
use RuntimeException;
use Throwable;
use WeakReference;

final class RabbitMqFailedJobHandler
{
    public function __construct(private readonly Closure $callback) {}

    /**
     * @param array<string, mixed> $data
     */
    public function failed(array $data, ?Throwable $exception, string $uuid, mixed $job): void
    {
        ($this->callback)($data, $exception, $uuid, $job);
    }
}

final class RabbitMqJobTest extends TestCase
{
    public function testItExposesTheNativePayloadIdentifierAndAttempts(): void
    {
        $delivery = $this->delivery(attempts: 4);
        $job = $this->job($delivery);

        self::assertSame($delivery->payload(), $job->getRawBody());
        self::assertSame('018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137', $job->getJobId());
        self::assertSame(4, $job->attempts());
        self::assertSame('rabbit-main', $job->getConnectionName());
        self::assertSame('orders.high', $job->getQueue());
    }

    public function testQueueMarshalsADeliveryWithItsLaravelContext(): void
    {
        $queue = new RabbitMqQueue(new Pool(), [
            'default' => [
                'broker' => 'default-broker',
                'exchange' => 'jobs',
                'routing_key' => '{queue}',
            ],
        ], 'main');
        $queue->setContainer($this->app);
        $queue->setConnectionName('rabbit-main');

        $job = $queue->marshalJob($this->delivery(), 'orders.high');

        self::assertSame('rabbit-main', $job->getConnectionName());
        self::assertSame('orders.high', $job->getQueue());
    }

    public function testDeleteAcknowledgesExactlyOnceAndKeepsCachedMetadata(): void
    {
        $delivery = $this->delivery(attempts: 3);
        $job = $this->job($delivery);

        $job->delete();
        $job->delete();

        self::assertSame(1, $delivery->ackCalls);
        self::assertTrue($job->isDeleted());
        self::assertSame($delivery->payload(), $job->getRawBody());
        self::assertSame('018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137', $job->getJobId());
        self::assertSame(3, $job->attempts());
    }

    public function testDeleteReleasesTheNativeHandleAfterAcknowledgement(): void
    {
        $delivery = $this->delivery();
        $reference = WeakReference::create($delivery);
        $job = $this->job($delivery);

        $job->delete();
        unset($delivery);
        gc_collect_cycles();

        self::assertNull($reference->get());
    }

    public function testReleaseWithoutDelayRequeuesThroughTheNativeHandle(): void
    {
        $delivery = $this->delivery();
        $job = $this->job($delivery);

        $job->release(0);

        self::assertSame([0], $delivery->releaseDelays);
        self::assertTrue($job->isReleased());
        self::assertFalse($job->isDeleted());
    }

    public function testReleaseConvertsLaravelSecondsToNativeMilliseconds(): void
    {
        $delivery = $this->delivery();
        $job = $this->job($delivery);

        $job->release(10);

        self::assertSame([10_000], $delivery->releaseDelays);
        self::assertTrue($job->isReleased());
    }

    public function testAckConnectionFailureIsPropagatedWithoutMarkingTheJobDeleted(): void
    {
        $delivery = $this->delivery();
        $native = new ConnectionException('delivery belongs to a stale connection generation');
        $delivery->throwOnNextAck($native);
        $job = $this->job($delivery);

        try {
            $job->delete();
            self::fail('The native ACK failure was not propagated.');
        } catch (ConnectionException $exception) {
            self::assertSame($native, $exception);
        }

        self::assertSame(1, $delivery->ackCalls);
        self::assertFalse($job->isDeleted());
    }

    public function testFailUsesTheLaravelAckCallbackAndEventSequence(): void
    {
        $order = [];
        $delivery = $this->delivery();
        $delivery->onAck(static function () use (&$order): void {
            $order[] = 'ack';
        });
        $job = $this->job($delivery);
        $failure = new RuntimeException('job failed');
        $handler = new RabbitMqFailedJobHandler(
            function (array $data, ?Throwable $exception, string $uuid, mixed $failedJob) use (
                &$order,
                $delivery,
                $failure,
                $job,
            ): void {
                self::assertSame(1, $delivery->ackCalls);
                self::assertSame(['report' => 42], $data);
                self::assertSame($failure, $exception);
                self::assertSame('018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137', $uuid);
                self::assertSame($job, $failedJob);
                $order[] = 'failed';
            },
        );
        $this->app->instance(RabbitMqFailedJobHandler::class, $handler);
        $events = $this->createMock(Dispatcher::class);
        $events->expects(self::once())
            ->method('dispatch')
            ->with(self::callback(function (mixed $event) use (&$order, $failure, $job): bool {
                self::assertInstanceOf(JobFailed::class, $event);
                self::assertSame('rabbit-main', $event->connectionName);
                self::assertSame($job, $event->job);
                self::assertSame($failure, $event->exception);
                $order[] = 'event';

                return true;
            }));
        $this->app->instance(Dispatcher::class, $events);

        $job->fail($failure);

        self::assertSame(['ack', 'failed', 'event'], $order);
        self::assertTrue($job->hasFailed());
        self::assertTrue($job->isDeleted());
    }

    private function job(Delivery $delivery): RabbitMqJob
    {
        return new RabbitMqJob(
            $this->app,
            $delivery,
            'rabbit-main',
            'orders.high',
        );
    }

    private function delivery(int $attempts = 1): Delivery
    {
        return new Delivery(
            json_encode([
                'uuid' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
                'job' => RabbitMqFailedJobHandler::class,
                'data' => ['report' => 42],
            ], JSON_THROW_ON_ERROR),
            [
                'message_id' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
                'subscription' => 'orders_high',
                'attempts' => $attempts,
                'state' => 'pending',
                'headers' => [],
            ],
        );
    }
}