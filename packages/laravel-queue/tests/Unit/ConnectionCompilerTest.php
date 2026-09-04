<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;

describe('connection compilation', function (): void {
    it('compiles a full connection to the exact native shape', function (): void {
        expect(ConnectionCompiler::compile('orders', fullConnection()))
            ->toBe(referenceCompiled('orders'));
    });

    it('applies defaults for every omitted key on a minimal connection', function (): void {
        expect(ConnectionCompiler::compile('orders', ['driver' => 'rabbit-rs', 'queue' => 'default']))
            ->toBe(referenceCompiled('orders'));
    });
});

describe('hosts', function (): void {
    it('parses flat and array host lists into sorted endpoints', function (array|string $hosts, array $expected): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => $hosts]);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe($expected);
    })->with([
        'flat comma-separated string, sorted' => [
            'rabbit-b:5673,rabbit-a',
            [hostEndpoint('rabbit-a', 5672), hostEndpoint('rabbit-b', 5673)],
        ],
        'flat string accepted verbatim' => [
            'a:5672,b:5672',
            [hostEndpoint('a', 5672), hostEndpoint('b', 5672)],
        ],
        'array of host strings' => [
            ['rabbit-b:5673', 'rabbit-a'],
            [hostEndpoint('rabbit-a', 5672), hostEndpoint('rabbit-b', 5673)],
        ],
    ]);

    it('parses a bracketed IPv6 endpoint', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => '[::1]:5672']);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe([hostEndpoint('::1', 5672)]);
    });

    it('rejects an out-of-range port with the exact path', function (): void {
        expectCompileRejected(['hosts' => '127.0.0.1:70000'], 'queue.connections.orders.hosts.0');
    });

    it('rejects empty hosts with the exact path', function (): void {
        expectCompileRejected(['hosts' => []], 'queue.connections.orders.hosts');
    });
});

describe('env booleans', function (): void {
    it('accepts every env-style boolean spelling on best_effort', function (bool|string|null $value, bool $expected): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'best_effort' => $value]);

        expect($compiled['best_effort'])->toBe($expected);
    })->with([
        'true' => [true, true],
        '"1"' => ['1', true],
        '"true"' => ['true', true],
        '"on"' => ['on', true],
        '"yes"' => ['yes', true],
        'false' => [false, false],
        '"0"' => ['0', false],
        '"false"' => ['false', false],
        '"off"' => ['off', false],
        '"no"' => ['no', false],
        '""' => ['', false],
        'null falls back to the default' => [null, false],
    ]);

    it('rejects a junk boolean string with the exact path', function (): void {
        expectCompileRejected(['best_effort' => 'maybe'], 'queue.connections.orders.best_effort');
    });

    it('casts env booleans on the other boolean keys', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'auto_subscribe' => 'off',
            'queue_durable' => 'no',
            'tls' => ['enabled' => 'yes'],
        ]);

        expect($compiled['auto_subscribe'])->toBeFalse()
            ->and($compiled['native']['queue_durable'])->toBeFalse()
            ->and($compiled['native']['brokers'][0]['tls']['enabled'])->toBeTrue();
    });

    it('falls back to the default when auto_subscribe is null', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'auto_subscribe' => null]);

        expect($compiled['auto_subscribe'])->toBeTrue();
    });
});

describe('env integers', function (): void {
    it('casts env-style integer strings', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'prefetch' => '64',
            'wait_timeout' => '5000',
            'confirm_timeout' => '30000',
        ]);

        expect($compiled['native']['workers'][0]['subscriptions'][0]['prefetch'])->toBe(64)
            ->and($compiled['native']['consumer']['wait_timeout'])->toBe(5000)
            ->and($compiled['publisher']['confirm_timeout'])->toBe(30000);
    });

    it('rejects a junk integer string with the exact path', function (): void {
        expectCompileRejected(['prefetch' => 'abc'], 'queue.connections.orders.prefetch');
    });

    it('range-checks cast integers', function (): void {
        expectCompileRejected(['heartbeat' => '-1'], 'queue.connections.orders.heartbeat');
    });
});

describe('publisher safety', function (): void {
    it('derives safety and confirms; keeps the deprecated mandatory field true', function (string $safety, bool $confirms, bool $mandatory): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'safety' => $safety]);

        expect($compiled['publisher'])->toBe(array_merge(
            referenceCompiled('orders')['publisher'],
            ['safety' => $safety, 'confirms' => $confirms, 'mandatory' => $mandatory],
        ))->and($compiled['native']['publisher'])->toBe($compiled['publisher']);
    })->with([
        // mandatory must always compile to true: the core config (Round G #78)
        // rejects mandatory=false — publisher.safety is the only wire-level
        // opt-out (safe confirms+mandatory, unsafe confirms-only, blind neither).
        'safe' => ['safe', true, true],
        'unsafe' => ['unsafe', true, true],
        'blind' => ['blind', false, true],
    ]);

    it('rejects an unknown safety mode with the exact path', function (): void {
        expectCompileRejected(['safety' => 'careless'], 'queue.connections.orders.safety');
    });
});

describe('unknown keys', function (): void {
    it('rejects unknown keys inside known sections with the exact path', function (string $section, array $value, string $expectedPath): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', $section => $value]))
            ->toThrow(InvalidArgumentException::class, $expectedPath);
    })->with([
        'tls' => ['tls', ['enabled' => false, 'hosts_extra' => 'x'], 'queue.connections.orders.tls.hosts_extra'],
        'delay' => ['delay', ['mode' => 'auto', 'junk' => 1], 'queue.connections.orders.delay.junk'],
        'dead_letter' => ['dead_letter', ['exchange' => 'dlx', 'queue' => 'dlq', 'junk' => 1], 'queue.connections.orders.dead_letter.junk'],
    ]);
});

describe('queue key', function (): void {
    it('requires a non-empty string queue', function (array $config): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', $config))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.queue');
    })->with([
        'missing' => [['driver' => 'rabbit-rs']],
        'empty' => [['driver' => 'rabbit-rs', 'queue' => '']],
        'not a string' => [['driver' => 'rabbit-rs', 'queue' => 123]],
    ]);
});

describe('bounds', function (): void {
    it('bounds wait_timeout between 1000 and 86400000', function (int $value, bool $valid): void {
        expectBounded(
            fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'wait_timeout' => $value]),
            fn (array $compiled): int => $compiled['native']['consumer']['wait_timeout'],
            $value,
            $valid,
            'queue.connections.orders.wait_timeout',
        );
    })->with([
        'below the minimum' => [999, false],
        'at the minimum' => [1000, true],
        'at the maximum' => [86_400_000, true],
        'beyond the maximum' => [86_400_001, false],
    ]);

    it('bounds prefetch between 1 and 65535', function (int $value, bool $valid): void {
        expectBounded(
            fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'prefetch' => $value]),
            fn (array $compiled): int => $compiled['native']['workers'][0]['subscriptions'][0]['prefetch'],
            $value,
            $valid,
            'queue.connections.orders.prefetch',
        );
    })->with([
        'zero' => [0, false],
        'at the maximum' => [65_535, true],
        'beyond the maximum' => [65_536, false],
    ]);

    it('bounds confirm_timeout to at least 1000', function (int $value, bool $valid): void {
        expectBounded(
            fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'confirm_timeout' => $value]),
            fn (array $compiled): int => $compiled['publisher']['confirm_timeout'],
            $value,
            $valid,
            'queue.connections.orders.confirm_timeout',
        );
    })->with([
        'below the minimum' => [999, false],
        'at the minimum' => [1000, true],
    ]);
});

describe('management url', function (): void {
    it('accepts null and blank values without propagating them', function (mixed $value): void {
        $withKey = ConnectionCompiler::compile('orders', ['queue' => 'default', 'management_url' => $value]);
        $withoutKey = ConnectionCompiler::compile('orders', ['queue' => 'default']);

        expect($withKey)->toBe($withoutKey);
    })->with([null, '', '   ', 'https://mq.local:15672']);

    it('rejects a non-string management_url with the exact path', function (): void {
        expectCompileRejected(['management_url' => 15672], 'queue.connections.orders.management_url');
    });
});

describe('topology', function (): void {
    it('rejects delivery_limit without dead_letter', function (): void {
        expectCompileRejected(['delivery_limit' => 20], 'queue.connections.orders.dead_letter');
    });

    it('propagates delivery_limit with dead_letter into the native config', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'delivery_limit' => 20,
            'dead_letter' => ['exchange' => 'dlx', 'queue' => 'dlq'],
        ]);

        expect($compiled['native']['delivery_limit'])->toBe(20)
            ->and($compiled['native']['dead_letter'])->toBe([
                'enabled' => true,
                'exchange' => 'dlx',
                'queue' => 'dlq',
                'routing_key' => null,
            ])
            ->and($compiled['topology']['dead_letter'])->toBe($compiled['native']['dead_letter']);
    });

    it('rejects an unknown topology_mode with the exact path', function (): void {
        expectCompileRejected(['topology_mode' => 'managed'], 'queue.connections.orders.topology_mode');
    });

    it('rejects an unknown queue_type with the exact path', function (): void {
        expectCompileRejected(['queue_type' => 'lazy'], 'queue.connections.orders.queue_type');
    });
});

describe('credentials and tls', function (): void {
    it('maps explicit credentials and vhost', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'vhost' => '/eu', 'username' => 'app', 'password' => '']);

        expect($compiled['native']['brokers'][0]['vhost'])->toBe('/eu')
            ->and($compiled['native']['brokers'][0]['credentials'])->toBe(['username' => 'app', 'password' => '']);
    });

    it('allows an empty password but rejects an empty username', function (): void {
        expectCompileRejected(['username' => ''], 'queue.connections.orders.username');
    });

    it('rejects a non-string tls certificate path', function (): void {
        expectCompileRejected(['tls' => ['ca_cert' => 5]], 'queue.connections.orders.tls.ca_cert');
    });
});

describe('delay', function (): void {
    it('rejects an unknown delay mode with the exact path', function (): void {
        expectCompileRejected(['delay' => ['mode' => 'cron']], 'queue.connections.orders.delay.mode');
    });

    it('rejects empty delay buckets with the exact path', function (): void {
        expectCompileRejected(['delay' => ['buckets' => []]], 'queue.connections.orders.delay.buckets');
    });
});

describe('package defaults', function (): void {
    it('fills every gap from the package defaults', function (): void {
        expect(ConnectionCompiler::compile('orders', ['queue' => 'default'], packageDefaults()))
            ->toBe(referenceCompiled('orders'));
    });

    it('lets the connection value win over the package default for scalars', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'prefetch' => 7,
            'safety' => 'blind',
        ], array_merge(packageDefaults(), ['prefetch' => 100, 'safety' => 'safe']));

        expect($compiled['native']['workers'][0]['subscriptions'][0]['prefetch'])->toBe(7)
            ->and($compiled['publisher']['safety'])->toBe('blind')
            ->and($compiled['publisher']['confirms'])->toBeFalse();
    });

    it('merges the delay section per sub-key', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'delay' => ['mode' => 'ttl'],
        ], ['delay' => ['buckets' => [10, 20, 30]]]);

        expect($compiled['native']['delay'])->toBe([
            'mode' => 'ttl',
            'buckets' => [10, 20, 30],
            'max_buckets' => 8,
            'queue_expiry_margin' => 60,
        ]);
    });

    it('lets connection sub-keys win over package default sub-keys', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'delay' => ['mode' => 'ttl', 'buckets' => [60]],
        ], ['delay' => ['mode' => 'plugin', 'buckets' => [10, 20], 'queue_expiry_margin' => 5]]);

        expect($compiled['native']['delay'])->toBe([
            'mode' => 'ttl',
            'buckets' => [60],
            'max_buckets' => 8,
            'queue_expiry_margin' => 5,
        ]);
    });

    it('merges the tls section per sub-key', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'tls' => ['enabled' => 'true'],
        ], ['tls' => [
            'enabled' => false,
            'ca_cert' => '/etc/rabbit/ca.pem',
            'client_cert' => '/etc/rabbit/client.pem',
            'client_key' => '/etc/rabbit/client.key',
        ]]);

        expect($compiled['native']['brokers'][0]['tls'])->toBe([
            'enabled' => true,
            'ca_cert' => '/etc/rabbit/ca.pem',
            'client_cert' => '/etc/rabbit/client.pem',
            'client_key' => '/etc/rabbit/client.key',
        ]);
    });

    it('merges the dead_letter section per sub-key', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'dead_letter' => ['exchange' => 'orders.dlx'],
        ], ['dead_letter' => ['queue' => 'orders.dlq', 'routing_key' => 'dead']]);

        expect($compiled['native']['dead_letter'])->toBe([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.dlq',
            'routing_key' => 'dead',
        ]);
    });

    it('passes unknown top-level default keys through when the connection omits them', function (): void {
        expect(ConnectionCompiler::compile('orders', ['queue' => 'default'], [
            'production_warning' => true,
            'worker' => ['strategy' => 'weighted_fair'],
        ]))->toBe(referenceCompiled('orders'));
    });

    it('tolerates a connection key the package defaults already define', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'worker' => ['custom']], [
            'worker' => ['strategy' => 'weighted_fair'],
        ]);

        expect($compiled)->toBe(referenceCompiled('orders'));
    });

    it('rejects an unknown top-level connection key that no default covers', function (): void {
        expectCompileRejected(['prefetchh' => 10], 'queue.connections.orders.prefetchh: unknown key', packageDefaults());
    });

    it('casts env-style default values after the merge', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default'], [
            'confirm_timeout' => '30000',
            'wait_timeout' => '5000',
            'heartbeat' => '15',
            'best_effort' => '1',
            'prefetch' => '32',
            'auto_subscribe' => '0',
        ]);

        expect($compiled['publisher']['confirm_timeout'])->toBe(30000)
            ->and($compiled['native']['consumer']['wait_timeout'])->toBe(5000)
            ->and($compiled['native']['brokers'][0]['heartbeat'])->toBe(15)
            ->and($compiled['best_effort'])->toBeTrue()
            ->and($compiled['native']['workers'][0]['subscriptions'][0]['prefetch'])->toBe(32)
            ->and($compiled['auto_subscribe'])->toBeFalse();
    });

    it('applies section validation after defaults fill the gaps', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default'], array_merge(packageDefaults(), [
            'delivery_limit' => 20,
            'dead_letter' => ['exchange' => 'dlx', 'queue' => 'dlq'],
        ]));

        expect($compiled['native']['delivery_limit'])->toBe(20)
            ->and($compiled['native']['dead_letter']['exchange'])->toBe('dlx');
    });

    it('rejects a default delivery_limit whose dead_letter the connection nulls out', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'dead_letter' => null], array_merge(packageDefaults(), [
            'delivery_limit' => 20,
            'dead_letter' => ['exchange' => 'dlx', 'queue' => 'dlq'],
        ])))->toThrow(InvalidArgumentException::class, 'queue.connections.orders.dead_letter');
    });
});

describe('subscriptions escape hatch', function (): void {
    it('replaces the derived subscription with the escape-hatch list', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'best_effort' => true,
            'subscriptions' => [
                'jobs' => ['queue' => 'orders.jobs'],
                'alerts' => [
                    'queue' => 'orders.alerts',
                    'weight' => 3,
                    'priority_class' => 2,
                    'prefetch' => 8,
                    'starvation_after' => 10,
                    'early_ack' => true,
                    'no_ack' => false,
                ],
            ],
        ]);

        expect($compiled['native']['workers'][0]['subscriptions'])->toBe([
            subscription('jobs'),
            subscription('alerts', [
                'weight' => 3,
                'priority_class' => 2,
                'prefetch' => 8,
                'starvation_after' => 10,
                'early_ack' => true,
            ]),
        ]);
    });

    it('falls back to the connection prefetch and casts env integers per subscription', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'prefetch' => '32',
            'subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'weight' => '2', 'priority_class' => '1']],
        ]);

        expect($compiled['native']['workers'][0]['subscriptions'][0])->toBe(
            subscription('jobs', ['weight' => 2, 'priority_class' => 1, 'prefetch' => 32]),
        );
    });

    it('rejects an empty subscriptions array', function (): void {
        expectCompileRejected(['subscriptions' => []], 'queue.connections.orders.subscriptions');
    });

    it('rejects unknown keys inside a subscription', function (): void {
        expectCompileRejected(
            ['subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'enabled' => true]]],
            'queue.connections.orders.subscriptions.jobs.enabled: unknown key',
        );
    });

    it('requires a non-empty queue per subscription', function (): void {
        expectCompileRejected(
            ['subscriptions' => ['jobs' => []]],
            'queue.connections.orders.subscriptions.jobs.queue',
        );
    });

    it('rejects an empty subscription alias', function (): void {
        expectCompileRejected(
            ['subscriptions' => ['' => ['queue' => 'orders.jobs']]],
            'queue.connections.orders.subscriptions',
        );
    });

    it('rejects early_ack without best_effort with the exact message', function (): void {
        expectCompileRejected(
            ['subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'early_ack' => true]]],
            'queue.connections.orders.subscriptions.jobs.early_ack: early_ack is not allowed in reliable mode — set best_effort=true to opt in',
        );
    });

    it('rejects no_ack without early_ack with the exact message', function (): void {
        expectCompileRejected(
            ['best_effort' => true, 'subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'no_ack' => true]]],
            "queue.connections.orders.subscriptions.jobs.no_ack: no_ack=true requires early_ack=true for subscription 'jobs'",
        );
    });

    it('fires the early_ack guard before the no_ack best_effort guard', function (): void {
        // Guard precedence: the no_ack best_effort message is unreachable
        // when early_ack already lacks best_effort.
        expectCompileRejected(
            ['subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'early_ack' => true, 'no_ack' => true]]],
            'queue.connections.orders.subscriptions.jobs.early_ack: early_ack is not allowed in reliable mode — set best_effort=true to opt in',
        );
    });

    it('accepts the full no_ack combination when best_effort opts in', function (): void {
        $compiled = ConnectionCompiler::compile('orders', [
            'queue' => 'default',
            'best_effort' => true,
            'subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'early_ack' => true, 'no_ack' => true]],
        ]);

        expect($compiled['native']['workers'][0]['subscriptions'][0]['early_ack'])->toBeTrue()
            ->and($compiled['native']['workers'][0]['subscriptions'][0]['no_ack'])->toBeTrue();
    });

    it('bounds weight between 1 and 65535', function (int $value, bool $valid): void {
        expectBounded(
            fn (): array => ConnectionCompiler::compile('orders', [
                'queue' => 'default',
                'subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'weight' => $value]],
            ]),
            fn (array $compiled): int => $compiled['native']['workers'][0]['subscriptions'][0]['weight'],
            $value,
            $valid,
            'queue.connections.orders.subscriptions.jobs.weight',
        );
    })->with([
        'zero' => [0, false],
        'at the maximum' => [65_535, true],
        'beyond the maximum' => [65_536, false],
    ]);

    it('bounds priority_class to i16', function (int $value, bool $valid): void {
        expectBounded(
            fn (): array => ConnectionCompiler::compile('orders', [
                'queue' => 'default',
                'subscriptions' => ['jobs' => ['queue' => 'orders.jobs', 'priority_class' => $value]],
            ]),
            fn (array $compiled): int => $compiled['native']['workers'][0]['subscriptions'][0]['priority_class'],
            $value,
            $valid,
            'queue.connections.orders.subscriptions.jobs.priority_class',
        );
    })->with([
        'at the minimum' => [-32_768, true],
        'at the maximum' => [32_767, true],
        'below the minimum' => [-32_769, false],
        'beyond the maximum' => [32_768, false],
    ]);

    it('rejects two subscriptions sharing the same queue', function (): void {
        expectCompileRejected(
            ['subscriptions' => [
                'jobs' => ['queue' => 'orders.jobs'],
                'mirror' => ['queue' => 'orders.jobs'],
            ]],
            'queue.connections.orders.subscriptions.mirror.queue',
        );
    });
});

/**
 * Package defaults as the service provider will feed them: the package
 * config minus brokers, routes, and workers — including keys the compiler
 * passes through (worker, production_warning).
 *
 * @return array<string, mixed>
 */
function packageDefaults(): array
{
    return [
        'heartbeat' => 30,
        'tls' => ['enabled' => false, 'ca_cert' => null, 'client_cert' => null, 'client_key' => null],
        'safety' => 'safe',
        'confirm_timeout' => 30000,
        'prefetch' => 64,
        'wait_timeout' => 30000,
        'topology_mode' => 'declare',
        'queue_type' => 'quorum',
        'queue_durable' => true,
        'delivery_limit' => null,
        'dead_letter' => null,
        'delay' => ['mode' => 'auto', 'buckets' => [1, 5, 30, 120], 'max_buckets' => 8, 'queue_expiry_margin' => 60],
        'worker' => ['strategy' => 'weighted_fair'],
        'auto_subscribe' => true,
        'production_warning' => true,
        'best_effort' => false,
    ];
}

/**
 * A compiled broker endpoint.
 *
 * @return array{host: string, port: int}
 */
function hostEndpoint(string $host, int $port): array
{
    return ['host' => $host, 'port' => $port];
}

/**
 * Asserts the compiler rejects a connection overriding the given keys with
 * the exact argument-exception path.
 *
 * @param array<string, mixed> $override
 */
function expectCompileRejected(array $override, string $path, ?array $defaults = null): void
{
    expect(fn (): array => ConnectionCompiler::compile('orders', array_merge(fullConnection(), $override), $defaults ?? []))
        ->toThrow(InvalidArgumentException::class, $path);
}

/**
 * Asserts a bounded integer setting: valid values land in the compiled config
 * at the given path, invalid ones throw with the exact compiler path.
 *
 * @param callable(): array $compile
 * @param callable(array): int $read
 */
function expectBounded(callable $compile, callable $read, int $value, bool $valid, string $path): void
{
    if ($valid) {
        expect($read($compile()))->toBe($value);
    } else {
        expect($compile)->toThrow(InvalidArgumentException::class, $path);
    }
}

/**
 * A compiled subscription row for the reference 'orders' broker.
 *
 * @param array<string, mixed> $overrides
 * @return array<string, mixed>
 */
function subscription(string $name, array $overrides = []): array
{
    return array_merge([
        'name' => $name,
        'broker' => 'orders',
        'queue' => 'orders.' . $name,
        'weight' => 1,
        'priority_class' => 0,
        'prefetch' => 64,
        'starvation_after' => 30,
        'early_ack' => false,
        'no_ack' => false,
    ], $overrides);
}

/**
 * @return array<string, mixed>
 */
function fullConnection(): array
{
    return [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'hosts' => '127.0.0.1:5672',
        'vhost' => '/',
        'username' => 'guest',
        'password' => 'guest',
        'heartbeat' => 30,
        'tls' => ['enabled' => false, 'ca_cert' => null, 'client_cert' => null, 'client_key' => null],
        'exchange' => 'laravel.jobs',
        'routing_key' => '{queue}',
        'safety' => 'safe',
        'confirm_timeout' => 30000,
        'prefetch' => 64,
        'wait_timeout' => 30000,
        'max_attempts' => 20,
        'best_effort' => false,
        'auto_subscribe' => true,
        'topology_mode' => 'declare',
        'queue_type' => 'quorum',
        'queue_durable' => true,
        'delivery_limit' => null,
        'dead_letter' => null,
        'delay' => ['mode' => 'auto', 'buckets' => [1, 5, 30, 120], 'max_buckets' => 8, 'queue_expiry_margin' => 60],
    ];
}

/**
 * @return array<string, mixed>
 */
function referenceCompiled(string $name): array
{
    return [
        'native' => [
            'brokers' => [[
                'name' => $name,
                'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
                'vhost' => '/',
                'credentials' => ['username' => 'guest', 'password' => 'guest'],
                'tls' => ['enabled' => false, 'ca_cert' => null, 'client_cert' => null, 'client_key' => null],
                'heartbeat' => 30,
            ]],
            'workers' => [[
                'name' => $name,
                'subscriptions' => [[
                    'name' => 'default',
                    'broker' => $name,
                    'queue' => 'default',
                    'weight' => 1,
                    'priority_class' => 0,
                    'prefetch' => 64,
                    'starvation_after' => 30,
                    'early_ack' => false,
                    'no_ack' => false,
                ]],
                'scheduler' => ['strategy' => 'weighted_fair'],
            ]],
            'topology_mode' => 'declare',
            'delay' => ['mode' => 'auto', 'buckets' => [1, 5, 30, 120], 'max_buckets' => 8, 'queue_expiry_margin' => 60],
            'dead_letter' => null,
            'delivery_limit' => null,
            'publisher' => ['safety' => 'safe', 'confirms' => true, 'mandatory' => true, 'confirm_timeout' => 30000],
            'consumer' => ['wait_timeout' => 30000, 'max_attempts' => 20],
            'queue_type' => 'quorum',
            'queue_durable' => true,
        ],
        'routes' => ['default' => ['broker' => $name, 'exchange' => 'laravel.jobs', 'routing_key' => '{queue}']],
        'publisher' => ['safety' => 'safe', 'confirms' => true, 'mandatory' => true, 'confirm_timeout' => 30000],
        'topology' => ['queue' => ['type' => 'quorum', 'durable' => true, 'delivery_limit' => null], 'dead_letter' => null],
        'best_effort' => false,
        'auto_subscribe' => true,
    ];
}
