<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Event;

function makeQueueWithPool($app): array
{
    $pool = new Pool();
    $queue = new RabbitMqQueue($pool, makeRoutes(), 'default');
    $queue->setContainer($app);

    return [$queue, $pool];
}

function makeRoutes(): array
{
    return [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'default.jobs',
            'routing_key' => '{queue}',
        ],
    ];
}

describe('connection state events', function () {
    it('connection lost dispatches recovering state event', function () {
        Event::fake();

        [$queue, $pool] = makeQueueWithPool($this->app);

        $pool->simulateConnectionState('default', 'recovering', 1);

        Event::assertDispatched(ConnectionStateChanged::class, function (ConnectionStateChanged $event): bool {
            return $event->broker === 'default'
                && $event->state === 'recovering'
                && $event->generation === 1;
        });
    });

    it('connection restored dispatches ready state event with incremented generation', function () {
        Event::fake();

        [$queue, $pool] = makeQueueWithPool($this->app);

        $pool->simulateConnectionState('default', 'ready', 2);

        Event::assertDispatched(ConnectionStateChanged::class, function (ConnectionStateChanged $event): bool {
            return $event->broker === 'default'
                && $event->state === 'ready'
                && $event->generation === 2;
        });
    });
});

describe('backpressure events', function () {
    it('backpressure dispatches BackpressureDetected event', function () {
        Event::fake();

        [$queue, $pool] = makeQueueWithPool($this->app);

        $pool->simulateBackpressure('default', 256, 8192);

        Event::assertDispatched(BackpressureDetected::class, function (BackpressureDetected $event): bool {
            return $event->broker === 'default'
                && $event->inFlight === 256
                && $event->capacity === 8192;
        });
    });

    it('events are dispatched through Laravel event system', function () {
        Event::fake();

        [$queue, $pool] = makeQueueWithPool($this->app);

        $pool->simulateConnectionState('default', 'recovering', 1);
        $pool->simulateBackpressure('default', 128, 8192);

        Event::assertDispatched(ConnectionStateChanged::class);
        Event::assertDispatched(BackpressureDetected::class);
    });
});

describe('custom callbacks', function () {
    it('custom connection state callback overrides default event dispatch', function () {
        Event::fake();

        $pool = new Pool();
        $queue = new RabbitMqQueue($pool, makeRoutes(), 'default');
        $queue->setContainer($this->app);

        $called = false;
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$called): void {
            $called = true;
            expect($broker)->toBe('custom')
                ->and($state)->toBe('recovering')
                ->and($generation)->toBe(5);
        });

        $pool->simulateConnectionState('custom', 'recovering', 5);

        expect($called)->toBeTrue();
        Event::assertNotDispatched(ConnectionStateChanged::class);
    });

    it('custom backpressure callback overrides default event dispatch', function () {
        Event::fake();

        $pool = new Pool();
        $queue = new RabbitMqQueue($pool, makeRoutes(), 'default');
        $queue->setContainer($this->app);

        $called = false;
        $pool->onBackpressure(function (string $broker, int $inFlight, int $capacity) use (&$called): void {
            $called = true;
            expect($broker)->toBe('custom')
                ->and($inFlight)->toBe(512)
                ->and($capacity)->toBe(8192);
        });

        $pool->simulateBackpressure('custom', 512, 8192);

        expect($called)->toBeTrue();
        Event::assertNotDispatched(BackpressureDetected::class);
    });
});
