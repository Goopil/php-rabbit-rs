<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;

/**
 * Workers whose single profile is named after the connection ('orders') and
 * subscribes to orders.jobs: no profile subscribes to a queue named
 * 'default' and no profile is named 'default'.
 *
 * @return list<array<string, mixed>>
 */
function popFallbackWorkers(): array
{
    return [
        [
            'name' => 'orders',
            'subscriptions' => [
                ['name' => 'jobs', 'queue' => 'orders.jobs'],
            ],
        ],
    ];
}

/**
 * @return array<string, array<string, string>>
 */
function popFallbackRoutes(): array
{
    return [
        'default' => [
            'broker' => 'orders-broker',
            'exchange' => '',
            'routing_key' => '{queue}',
        ],
    ];
}

beforeEach(function (): void {
    bootFakeNativeExtension($this->app);
});

describe('pop(null) fallback', function (): void {
    it('gives pop(null) the actionable error when the default queue is not a known profile', function (): void {
        // The audit's worker-crash shape: without the guard, pop(null) hands
        // the default queue name to the pool as a profile name and dies with
        // the native `unknown worker profile` error instead.
        $pool = new Pool(['workers' => popFallbackWorkers()]);
        $queue = new RabbitMqQueue(
            $pool,
            popFallbackRoutes(),
            'default',
            workerProfiles: new WorkerProfileResolver(popFallbackWorkers()),
        );
        $queue->setContainer(new Container);

        expect(fn () => $queue->pop())->toThrow(
            InvalidArgumentException::class,
            "No worker profile subscribes to queue 'default': declare it in "
            .'queue.connections.<name> (queue key or subscriptions).',
        )->and($pool->consumerProfiles)->toBe([]);
    });

    it('gives pop(null) on a subscriptions-only connection the actionable error', function (): void {
        // Without a `queue` key (the remediation for a connection whose
        // queues are all declared as subscriptions) the connector falls back
        // to the default queue name 'default': not a subscription queue and
        // not the profile name ('rabbit-rs'), so pop(null) must fail with the
        // same actionable message an explicit pop of an unknown queue gets.
        config()->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'hosts' => 'localhost:5672',
            'subscriptions' => [
                'jobs' => ['queue' => 'orders.jobs'],
            ],
        ]);

        $queue = $this->app['queue']->connection('rabbit-rs');

        expect(fn () => $queue->pop())->toThrow(
            InvalidArgumentException::class,
            "No worker profile subscribes to queue 'default': declare it in "
            .'queue.connections.<name> (queue key or subscriptions).',
        );
    });

    it('keeps consuming the default queue on pop(null) when a subscription covers it', function (): void {
        config()->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'orders.jobs',
            'hosts' => 'localhost:5672',
            'subscriptions' => [
                'jobs' => ['queue' => 'orders.jobs'],
            ],
        ]);

        $queue = $this->app['queue']->connection('rabbit-rs');

        expect($queue->pop())->toBeNull();

        $pool = (new ReflectionProperty($queue, 'pool'))->getValue($queue); // @phpstan-ignore-line
        expect($pool->consumerProfiles)->toBe(['rabbit-rs']);
    });
});
