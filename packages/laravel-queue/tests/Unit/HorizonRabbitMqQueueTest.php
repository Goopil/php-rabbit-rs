<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqJob as HorizonRabbitMqJob;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Events\Dispatcher;
use Laravel\Horizon\Events\JobDeleted;
use Laravel\Horizon\Events\JobPending;
use Laravel\Horizon\Events\JobPushed;
use Laravel\Horizon\Events\JobReserved;

const HORIZON_EVENTS_NS = 'Laravel\Horizon\Events\\';

/**
 * @return list<array<string, mixed>>
 */
function horizonWorkers(): array
{
    return [
        [
            'name' => 'default',
            'subscriptions' => [
                ['name' => 'default', 'queue' => 'default'],
            ],
        ],
    ];
}

function horizonQueue(?Pool $pool = null): HorizonRabbitMqQueue
{
    $workers = horizonWorkers();
    $pool ??= new Pool(['workers' => $workers]);
    $resolver = new WorkerProfileResolver($workers);
    $queue = new HorizonRabbitMqQueue(
        $pool,
        [
            'default' => [
                'broker' => 'default-broker',
                'exchange' => 'jobs',
                'routing_key' => '{queue}',
            ],
        ],
        'default',
        workerProfiles: $resolver,
    );
    $queue->setContainer(app());
    $queue->setConnectionName('rabbit-rs');

    return $queue;
}

beforeEach(function (): void {
    $this->events = $this->createMock(Dispatcher::class);
    $this->app->instance(Dispatcher::class, $this->events);
});

it('dispatches JobPending then JobPushed on push', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void {
            if (str_starts_with($event::class, HORIZON_EVENTS_NS)) {
                $events[] = $event;
            }
        }
    );

    $queue->push('TestJob', ['key' => 'value'], 'orders');

    expect($events)->toHaveCount(2)
        ->and($events[0])->toBeInstanceOf(JobPending::class)
        ->and($events[1])->toBeInstanceOf(JobPushed::class)
        ->and($events[0]->queue)->toBe('orders')
        ->and($events[1]->queue)->toBe('orders')
        ->and($events[0]->connectionName)->toBe('rabbit-rs')
        ->and($events[1]->connectionName)->toBe('rabbit-rs');

    $payload = json_decode($events[0]->payload->value, true);
    expect($payload)->toHaveKey('type')
        ->and($payload)->toHaveKey('tags')
        ->and($payload)->toHaveKey('pushedAt');
});

it('dispatches JobPending then JobPushed on later', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void {
            if (str_starts_with($event::class, HORIZON_EVENTS_NS)) {
                $events[] = $event;
            }
        }
    );

    $queue->later(10, 'TestJob', ['key' => 'value'], 'orders');

    expect($events)->toHaveCount(2)
        ->and($events[0])->toBeInstanceOf(JobPending::class)
        ->and($events[1])->toBeInstanceOf(JobPushed::class);
});

it('dispatches JobReserved on pop when a job is returned', function (): void {
    $pool = new Pool(['workers' => horizonWorkers()]);
    $delivery = new Goopil\RabbitRs\Delivery(
        json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob', 'data' => []]),
        ['message_id' => 'test-uuid', 'subscription' => 'default', 'attempts' => 1, 'state' => 'pending', 'headers' => []],
    );
    $pool->pushDelivery('default', $delivery);

    $queue = horizonQueue($pool);
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void {
            if (str_starts_with($event::class, HORIZON_EVENTS_NS)) {
                $events[] = $event;
            }
        }
    );

    $job = $queue->pop('default');

    expect($job)->toBeInstanceOf(HorizonRabbitMqJob::class)
        ->and($events)->toHaveCount(1)
        ->and($events[0])->toBeInstanceOf(JobReserved::class)
        ->and($events[0]->queue)->toBe('default')
        ->and($events[0]->connectionName)->toBe('rabbit-rs');
});

it('does not dispatch any event on pop when no job is available', function (): void {
    $queue = horizonQueue();
    $dispatchCount = 0;
    $this->events->method('dispatch')->willReturnCallback(
        static function () use (&$dispatchCount): void { $dispatchCount++; }
    );

    $result = $queue->pop('default');

    expect($result)->toBeNull()
        ->and($dispatchCount)->toBe(0);
});

it('dispatches JobDeleted on deleteReserved', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void {
            if (str_starts_with($event::class, HORIZON_EVENTS_NS)) {
                $events[] = $event;
            }
        }
    );

    $job = $this->createMock(RabbitMqJob::class);
    $job->method('getRawBody')->willReturn(json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob']));

    $queue->deleteReserved('orders', $job);

    expect($events)->toHaveCount(1)
        ->and($events[0])->toBeInstanceOf(JobDeleted::class)
        ->and($events[0]->queue)->toBe('orders')
        ->and($events[0]->connectionName)->toBe('rabbit-rs');
});

it('marshalJob creates a HorizonRabbitMqJob', function (): void {
    $queue = horizonQueue();
    $delivery = new Goopil\RabbitRs\Delivery(
        json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob', 'data' => []]),
        ['message_id' => 'test-uuid', 'subscription' => 'default', 'attempts' => 1, 'state' => 'pending', 'headers' => []],
    );

    $job = $queue->marshalJob($delivery, 'orders');

    expect($job)->toBeInstanceOf(HorizonRabbitMqJob::class)
        ->and($job)->toBeInstanceOf(RabbitMqJob::class)
        ->and($job->getQueue())->toBe('orders');
});
