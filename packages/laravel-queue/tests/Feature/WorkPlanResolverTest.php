<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\WorkPlanResolver;

beforeEach(function (): void {
    config()->set('queue.connections', [
        'rabbit-rs' => [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
            'hosts' => 'localhost:5672',
        ],
        'redis' => [
            'driver' => 'redis',
            'queue' => 'default',
        ],
        'eu' => [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'eu-rabbit:5672',
            'subscriptions' => [
                'billing' => ['queue' => 'billing.events'],
                'mirror' => ['queue' => 'orders'],
            ],
        ],
        'us' => [
            'driver' => 'rabbit-rs',
            'queue' => 'orders',
            'hosts' => 'us-rabbit:5672',
        ],
    ]);
});

describe('WorkPlanResolver', function (): void {
    it('targets every rabbit-rs connection with all defined queues when no flag is given', function (): void {
        $plan = WorkPlanResolver::resolve(null, null);

        expect($plan)->toBe([
            ['connection' => 'rabbit-rs', 'queues' => ['default']],
            ['connection' => 'eu', 'queues' => ['orders', 'billing.events']],
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });

    it('filters connections while preserving config order', function (): void {
        $plan = WorkPlanResolver::resolve('us,eu', null);

        expect($plan)->toBe([
            ['connection' => 'eu', 'queues' => ['orders', 'billing.events']],
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });

    it('throws with the available list for an unknown connection', function (): void {
        expect(fn () => WorkPlanResolver::resolve('nope', null))
            ->toThrow(
                InvalidArgumentException::class,
                'Unknown rabbit-rs connection(s): nope. Available rabbit-rs connections: rabbit-rs, eu, us',
            );
    });

    it('resolves a queue by its subscription alias', function (): void {
        $plan = WorkPlanResolver::resolve(null, 'billing');

        expect($plan)->toBe([
            ['connection' => 'eu', 'queues' => ['billing.events']],
        ]);
    });

    it('consumes a queue defined on two connections on both', function (): void {
        $plan = WorkPlanResolver::resolve(null, 'orders');

        expect($plan)->toBe([
            ['connection' => 'eu', 'queues' => ['orders']],
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });

    it('resolves a mixed queue list on every connection that defines it', function (): void {
        $plan = WorkPlanResolver::resolve(null, 'orders,billing');

        expect($plan)->toBe([
            ['connection' => 'eu', 'queues' => ['orders', 'billing.events']],
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });

    it('throws with the defined queue names for an unknown queue', function (): void {
        expect(fn () => WorkPlanResolver::resolve(null, 'nope'))
            ->toThrow(
                InvalidArgumentException::class,
                'Unknown queue(s): nope. Defined queues: default, orders, billing.events',
            );
    });

    it('intersects connections and queues', function (): void {
        $plan = WorkPlanResolver::resolve('eu', 'orders,billing');

        expect($plan)->toBe([
            ['connection' => 'eu', 'queues' => ['orders', 'billing.events']],
        ]);
    });

    it('treats a queue no targeted connection defines as unknown', function (): void {
        expect(fn () => WorkPlanResolver::resolve('us', 'billing'))
            ->toThrow(
                InvalidArgumentException::class,
                'Unknown queue(s): billing. Defined queues: orders',
            );
    });

    it('throws when no rabbit-rs connection is configured', function (): void {
        config()->set('queue.connections', [
            'redis' => ['driver' => 'redis', 'queue' => 'default'],
        ]);

        expect(fn () => WorkPlanResolver::resolve(null, null))
            ->toThrow(InvalidArgumentException::class, 'No rabbit-rs queue connection is configured');

        expect(fn () => WorkPlanResolver::resolve('rabbit-rs', null))
            ->toThrow(InvalidArgumentException::class, 'No rabbit-rs queue connection is configured');
    });
});
