<?php

declare(strict_types=1);

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqJob as HorizonRabbitMqJob;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Events\Dispatcher;
use Laravel\Horizon\Events\JobDeleted;

function horizonDelivery(int $attempts = 1): Delivery
{
    return new Delivery(
        json_encode([
            'uuid' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
            'job' => 'TestJob',
            'data' => ['report' => 42],
        ], JSON_THROW_ON_ERROR),
        [
            'message_id' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
            'subscription' => 'default',
            'attempts' => $attempts,
            'state' => 'pending',
            'headers' => [],
        ],
    );
}

function horizonJob(Delivery $delivery, HorizonRabbitMqQueue $queue): HorizonRabbitMqJob
{
    return new HorizonRabbitMqJob(
        app(),
        $delivery,
        'rabbit-rs',
        'orders.high',
        $queue,
    );
}

function horizonQueueForJob(): HorizonRabbitMqQueue
{
    $queue = new HorizonRabbitMqQueue(new Pool(), [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'jobs',
            'routing_key' => '{queue}',
        ],
    ], 'default');
    $queue->setContainer(app());
    $queue->setConnectionName('rabbit-rs');

    return $queue;
}

it('calls deleteReserved on the queue after delete', function (): void {
    $queue = horizonQueueForJob();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $deleteReservedCalled = false;
    $events = $this->createMock(Dispatcher::class);
    $events->expects($this->once())
        ->method('dispatch')
        ->willReturnCallback(function (object $event) use (&$deleteReservedCalled): void {
            if ($event instanceof JobDeleted) {
                $deleteReservedCalled = true;
            }
        });
    $this->app->instance(Dispatcher::class, $events);

    $job->delete();

    expect($deleteReservedCalled)->toBeTrue()
        ->and($job->isDeleted())->toBeTrue()
        ->and($delivery->ackCalls)->toBe(1);
});

it('does not call deleteReserved when already deleted', function (): void {
    $queue = horizonQueueForJob();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $dispatchCount = 0;
    $events = $this->createMock(Dispatcher::class);
    $events->method('dispatch')->willReturnCallback(
        static function () use (&$dispatchCount): void { $dispatchCount++; }
    );
    $this->app->instance(Dispatcher::class, $events);

    $job->delete();
    $job->delete();

    expect($delivery->ackCalls)->toBe(1)
        ->and($job->isDeleted())->toBeTrue()
        ->and($dispatchCount)->toBe(1);
});

it('releases through the native delivery handle', function (): void {
    $queue = horizonQueueForJob();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $events = $this->createMock(Dispatcher::class);
    $this->app->instance(Dispatcher::class, $events);

    $job->release(5);

    expect([5_000])->toBe($delivery->releaseDelays)
        ->and($job->isReleased())->toBeTrue();
});

it('preserves job id, attempts, and raw body from parent', function (): void {
    $delivery = horizonDelivery(attempts: 3);
    $job = horizonJob($delivery, horizonQueueForJob());

    expect('018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137')->toBe($job->getJobId())
        ->and(3)->toBe($job->attempts())
        ->and($delivery->payload())->toBe($job->getRawBody())
        ->and('rabbit-rs')->toBe($job->getConnectionName())
        ->and('orders.high')->toBe($job->getQueue());
});
