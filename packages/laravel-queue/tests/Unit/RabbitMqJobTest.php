<?php

declare(strict_types=1);

use Goopil\RabbitRs\ConnectionException;
use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Events\Dispatcher;
use Illuminate\Queue\Events\JobFailed;

const RABBIT_MQ_JOB_TEST_MESSAGE_ID = '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137';

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

function job(Delivery $delivery): RabbitMqJob
{
    return new RabbitMqJob(
        app(),
        $delivery,
        'rabbit-main',
        'orders.high',
    );
}

function delivery(int $attempts = 1): Delivery
{
    return new Delivery(
        json_encode([
            'uuid' => RABBIT_MQ_JOB_TEST_MESSAGE_ID,
            'job' => RabbitMqFailedJobHandler::class,
            'data' => ['report' => 42],
        ], JSON_THROW_ON_ERROR),
        [
            'message_id' => RABBIT_MQ_JOB_TEST_MESSAGE_ID,
            'subscription' => 'orders_high',
            'attempts' => $attempts,
            'state' => 'pending',
            'headers' => [],
        ],
    );
}

it('exposes the native payload identifier and attempts', function (): void {
    $delivery = delivery(attempts: 4);
    $job = job($delivery);

    expect($delivery->payload())->toBe($job->getRawBody())
        ->and(RABBIT_MQ_JOB_TEST_MESSAGE_ID)->toBe($job->getJobId())
        ->and(4)->toBe($job->attempts())
        ->and('rabbit-main')->toBe($job->getConnectionName())
        ->and('orders.high')->toBe($job->getQueue());
});

it('marshals a delivery with its Laravel context', function (): void {
    $queue = new RabbitMqQueue(new Pool(), [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'jobs',
            'routing_key' => '{queue}',
        ],
    ], 'main');
    $queue->setContainer($this->app);
    $queue->setConnectionName('rabbit-main');

    $job = $queue->marshalJob(delivery(), 'orders.high');

    expect('rabbit-main')->toBe($job->getConnectionName())
        ->and('orders.high')->toBe($job->getQueue());
});

it('acknowledges exactly once and keeps cached metadata on delete', function (): void {
    $delivery = delivery(attempts: 3);
    $job = job($delivery);

    $job->delete();
    $job->delete();

    expect(1)->toBe($delivery->ackCalls)
        ->and($job->isDeleted())->toBeTrue()
        ->and($delivery->payload())->toBe($job->getRawBody())
        ->and(RABBIT_MQ_JOB_TEST_MESSAGE_ID)->toBe($job->getJobId())
        ->and(3)->toBe($job->attempts());
});

it('releases the native handle after acknowledgement', function (): void {
    $delivery = delivery();
    $reference = WeakReference::create($delivery);
    $job = job($delivery);

    $job->delete();
    unset($delivery);
    gc_collect_cycles();

    expect($reference->get())->toBeNull();
});

it('requeues through the native handle on release without delay', function (): void {
    $delivery = delivery();
    $job = job($delivery);

    $job->release(0);

    expect([0])->toBe($delivery->releaseDelays)
        ->and($job->isReleased())->toBeTrue()
        ->and($job->isDeleted())->toBeFalse();
});

it('converts Laravel seconds to native milliseconds on release', function (): void {
    $delivery = delivery();
    $job = job($delivery);

    $job->release(10);

    expect([10_000])->toBe($delivery->releaseDelays)
        ->and($job->isReleased())->toBeTrue();
});

it('propagates an ACK connection failure without marking the job deleted', function (): void {
    $delivery = delivery();
    $native = new ConnectionException('delivery belongs to a stale connection generation');
    $delivery->throwOnNextAck($native);
    $job = job($delivery);

    try {
        $job->delete();
        self::fail('The native ACK failure was not propagated.');
    } catch (ConnectionException $exception) {
        self::assertSame($native, $exception);
    }

    expect(1)->toBe($delivery->ackCalls)
        ->and($job->isDeleted())->toBeFalse();
});

it('uses the Laravel ack callback and event sequence on fail', function (): void {
    $order = [];
    $delivery = delivery();
    $delivery->onAck(static function () use (&$order): void {
        $order[] = 'ack';
    });
    $job = job($delivery);
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
            self::assertSame(RABBIT_MQ_JOB_TEST_MESSAGE_ID, $uuid);
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

    expect(['ack', 'failed', 'event'])->toBe($order)
        ->and($job->hasFailed())->toBeTrue()
        ->and($job->isDeleted())->toBeTrue();
});

it('throws InvalidArgumentException when message_id is missing', function (): void {
    $delivery = new Delivery(
        '{"job":"test"}',
        ['attempts' => 0],
    );

    expect(fn() => job($delivery))
        ->toThrow(InvalidArgumentException::class, 'message_id');
});

it('throws InvalidArgumentException when message_id is empty', function (): void {
    $delivery = new Delivery(
        '{"job":"test"}',
        ['message_id' => '', 'attempts' => 0],
    );

    expect(fn() => job($delivery))
        ->toThrow(InvalidArgumentException::class, 'message_id');
});

it('throws InvalidArgumentException when payload is invalid JSON', function (): void {
    $delivery = new Delivery(
        'not-json',
        ['message_id' => 'abc', 'attempts' => 0],
    );

    expect(fn() => job($delivery))
        ->toThrow(InvalidArgumentException::class, 'not valid JSON');
});
