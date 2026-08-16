<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Event;

final class NativeEventDispatchTest extends TestCase
{
    public function testConnectionLostDispatchesRecoveringStateEvent(): void
    {
        Event::fake();

        [$queue, $pool] = $this->queue();

        $pool->simulateConnectionState('default', 'recovering', 1);

        Event::assertDispatched(ConnectionStateChanged::class, function (ConnectionStateChanged $event): bool {
            return $event->broker === 'default'
                && $event->state === 'recovering'
                && $event->generation === 1;
        });
    }

    public function testConnectionRestoredDispatchesReadyStateEventWithIncrementedGeneration(): void
    {
        Event::fake();

        [$queue, $pool] = $this->queue();

        $pool->simulateConnectionState('default', 'ready', 2);

        Event::assertDispatched(ConnectionStateChanged::class, function (ConnectionStateChanged $event): bool {
            return $event->broker === 'default'
                && $event->state === 'ready'
                && $event->generation === 2;
        });
    }

    public function testBackpressureDispatchesBackpressureDetectedEvent(): void
    {
        Event::fake();

        [$queue, $pool] = $this->queue();

        $pool->simulateBackpressure('default', 256, 8192);

        Event::assertDispatched(BackpressureDetected::class, function (BackpressureDetected $event): bool {
            return $event->broker === 'default'
                && $event->inFlight === 256
                && $event->capacity === 8192;
        });
    }

    public function testEventsAreDispatchedThroughLaravelEventSystem(): void
    {
        Event::fake();

        [$queue, $pool] = $this->queue();

        $pool->simulateConnectionState('default', 'recovering', 1);
        $pool->simulateBackpressure('default', 128, 8192);

        Event::assertDispatched(ConnectionStateChanged::class);
        Event::assertDispatched(BackpressureDetected::class);
    }

    public function testCustomConnectionStateCallbackOverridesDefaultEventDispatch(): void
    {
        Event::fake();

        $pool = new Pool();
        $queue = new RabbitMqQueue($pool, $this->routes(), 'default');
        $queue->setContainer($this->app);

        $called = false;
        $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$called): void {
            $called = true;
            self::assertSame('custom', $broker);
            self::assertSame('recovering', $state);
            self::assertSame(5, $generation);
        });

        $pool->simulateConnectionState('custom', 'recovering', 5);

        self::assertTrue($called);
        Event::assertNotDispatched(ConnectionStateChanged::class);
    }

    public function testCustomBackpressureCallbackOverridesDefaultEventDispatch(): void
    {
        Event::fake();

        $pool = new Pool();
        $queue = new RabbitMqQueue($pool, $this->routes(), 'default');
        $queue->setContainer($this->app);

        $called = false;
        $pool->onBackpressure(function (string $broker, int $inFlight, int $capacity) use (&$called): void {
            $called = true;
            self::assertSame('custom', $broker);
            self::assertSame(512, $inFlight);
            self::assertSame(8192, $capacity);
        });

        $pool->simulateBackpressure('custom', 512, 8192);

        self::assertTrue($called);
        Event::assertNotDispatched(BackpressureDetected::class);
    }

    /**
     * @return array{RabbitMqQueue, Pool}
     */
    private function queue(): array
    {
        $pool = new Pool();
        $queue = new RabbitMqQueue($pool, $this->routes(), 'default');
        $queue->setContainer($this->app);

        return [$queue, $pool];
    }

    /**
     * @return array<string, array<string, string>>
     */
    private function routes(): array
    {
        return [
            'default' => [
                'broker' => 'default-broker',
                'exchange' => 'default.jobs',
                'routing_key' => '{queue}',
            ],
        ];
    }
}
