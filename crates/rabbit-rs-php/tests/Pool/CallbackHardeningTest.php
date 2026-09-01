<?php

declare(strict_types=1);

describe('callback hardening', function () {
    it('surfaces an exception thrown inside the connection state callback', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
        ]);

        $pool->onConnectionState(function (): void {
            throw new RuntimeException('alert pipeline down');
        });

        $publishAndFlush = static function () use ($pool): void {
            $pool->publish(pubMessage('cb-throw-1'));
            $pool->flush();
        };
        expect($publishAndFlush)->toThrow(
            RuntimeException::class,
            'alert pipeline down',
            'the callback exception must surface instead of being silently destroyed',
        );

        $pool->close();
    });

    it('preserves an exception thrown inside the backpressure callback', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publisher_capacity' => 1,
            'pending_confirmations' => 1,
        ]);

        $pool->onBackpressure(function (): void {
            throw new RuntimeException('backpressure alert failed');
        });

        try {
            $pool->publishBatch([
                pubMessage('bp-throw-1', 'payload', [], 1),
                pubMessage('bp-throw-2', 'payload', [], 1),
            ]);
            $this->fail('publishBatch must still surface its own backpressure error');
        } catch (\Goopil\RabbitRs\BackpressureException $exception) {
            // The operation's data-path error wins; the callback exception
            // must survive in the chain instead of being silently destroyed.
            $previous = $exception->getPrevious();
            expect($previous)->toBeInstanceOf(RuntimeException::class);
            expect($previous?->getMessage())->toBe('backpressure alert failed');
        }

        $pool->close();
    });

    it('invokes every registered connection state callback, not only the last one', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
        ]);

        $first = [];
        $second = [];
        $pool->onConnectionState(static function (string $broker, string $state, int $generation) use (&$first): void {
            $first[] = [$broker, $state, $generation];
        });
        // A second "connection" on the same native pool registers its own
        // callback (this is what NativePoolFactory does when two Laravel
        // connections share one fingerprint). Registration must not steal
        // the first callback.
        $pool->onConnectionState(static function (string $broker, string $state, int $generation) use (&$second): void {
            $second[] = [$broker, $state, $generation];
        });

        $pool->publish(pubMessage('cb-multi-1'));
        $pool->flush();

        expect($first)->not->toBeEmpty('the first registered callback must still fire');
        expect($second)->not->toBeEmpty('the second registered callback must fire');

        $pool->close();
    });

    it('removes every registered callback via clearEventCallbacks()', function () {
        $pool = testingPool(defaultConfigWithWorkers(), [
            'publication_outcomes' => ['ack'],
        ]);

        $fired = [];
        $pool->onConnectionState(static function (string $broker) use (&$fired): void {
            $fired[] = "state:{$broker}";
        });
        $pool->onBackpressure(static function (string $broker) use (&$fired): void {
            $fired[] = "backpressure:{$broker}";
        });
        $pool->onBackpressure(static function (string $broker) use (&$fired): void {
            $fired[] = "backpressure-2:{$broker}";
        });

        expect($pool->clearEventCallbacks())->toBe(3);

        $pool->publish(pubMessage('cb-clear-1'));
        $pool->flush();

        expect($fired)->toBeEmpty('cleared callbacks must not fire');

        $pool->close();
    });

    it('fires state callbacks on the fast path of Consumer::next()', function () {
        // Two worker profiles on distinct brokers: starting the second
        // coordinator mid-test produces a connection state that no drain has
        // recorded yet, while deliveries keep flowing (steady traffic).
        $config = [
            'brokers' => [
                [
                    'name' => 'first',
                    'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
                    'vhost' => '/',
                    'credentials' => ['username' => 'guest', 'password' => 'secret'],
                    'tls' => ['enabled' => false],
                    'heartbeat' => 30,
                ],
                [
                    'name' => 'second',
                    'hosts' => [['host' => '127.0.0.1', 'port' => 5673]],
                    'vhost' => '/',
                    'credentials' => ['username' => 'guest', 'password' => 'secret'],
                    'tls' => ['enabled' => false],
                    'heartbeat' => 30,
                ],
            ],
            'workers' => [
                [
                    'name' => 'main',
                    'subscriptions' => [[
                        'name' => 'first',
                        'broker' => 'first',
                        'queue' => 'jobs',
                        'weight' => 1,
                        'priority_class' => 0,
                        'prefetch' => 512,
                    ]],
                    'scheduler' => ['strategy' => 'weighted_fair', 'max_in_flight' => 512],
                ],
                [
                    'name' => 'second',
                    'subscriptions' => [[
                        'name' => 'second',
                        'broker' => 'second',
                        'queue' => 'jobs',
                        'weight' => 1,
                        'priority_class' => 0,
                        'prefetch' => 512,
                    ]],
                    'scheduler' => ['strategy' => 'weighted_fair', 'max_in_flight' => 512],
                ],
            ],
            'topology_mode' => 'external',
        ];
        $pool = testingPool($config, [
            'deliveries' => [
                ['message_id' => 'cb-fast-1', 'payload' => 'payload'],
                ['message_id' => 'cb-fast-2', 'payload' => 'payload'],
            ],
        ]);

        $brokers = [];
        $pool->onConnectionState(static function (string $broker, string $state, int $generation) use (&$brokers): void {
            $brokers[$broker] = "{$state}@{$generation}";
        });

        $consumer = $pool->consumer('main');
        // Consume the first delivery: whether this call takes the fast or slow
        // path, it drains and records the first broker's state, and leaves the
        // runtime enough scheduling cycles to buffer the second delivery.
        expect($consumer->next(5))->not->toBeNull();

        // Start the second broker's coordinator: its Ready state is now
        // pending (no drain has seen it yet).
        $pool->consumer('second');

        // Steady traffic: a delivery is available, so next() must take the
        // fast path — and still drain the pending second-broker state.
        $delivery = $consumer->next(5);
        expect($delivery)->not->toBeNull('a delivery is available, next() must take the fast path');
        expect($brokers)->toHaveKey('second', 'ready@1', 'the second broker state must fire even when a delivery is returned');

        $pool->close();
    });
});
