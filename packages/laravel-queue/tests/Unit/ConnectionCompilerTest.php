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
    it('splits a flat comma-separated hosts string into sorted endpoints', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => 'rabbit-b:5673,rabbit-a']);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe([
            ['host' => 'rabbit-a', 'port' => 5672],
            ['host' => 'rabbit-b', 'port' => 5673],
        ]);
    });

    it('accepts the documented flat string form verbatim', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => 'a:5672,b:5672']);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe([
            ['host' => 'a', 'port' => 5672],
            ['host' => 'b', 'port' => 5672],
        ]);
    });

    it('accepts an array of host strings', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => ['rabbit-b:5673', 'rabbit-a']]);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe([
            ['host' => 'rabbit-a', 'port' => 5672],
            ['host' => 'rabbit-b', 'port' => 5673],
        ]);
    });

    it('parses a bracketed IPv6 endpoint', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => '[::1]:5672']);

        expect($compiled['native']['brokers'][0]['hosts'])->toBe([['host' => '::1', 'port' => 5672]]);
    });

    it('rejects an out-of-range port with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => '127.0.0.1:70000']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.hosts.0');
    });

    it('rejects empty hosts with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'hosts' => []]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.hosts');
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
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'best_effort' => 'maybe']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.best_effort');
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
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'prefetch' => 'abc']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.prefetch');
    });

    it('range-checks cast integers', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'heartbeat' => '-1']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.heartbeat');
    });
});

describe('publisher safety', function (): void {
    it('derives confirms and mandatory from the safety mode', function (string $safety, bool $confirms, bool $mandatory): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'safety' => $safety]);

        expect($compiled['publisher'])->toBe([
            'safety' => $safety,
            'confirms' => $confirms,
            'mandatory' => $mandatory,
            'confirm_timeout' => 30000,
        ])->and($compiled['native']['publisher'])->toBe($compiled['publisher']);
    })->with([
        'safe' => ['safe', true, true],
        'unsafe' => ['unsafe', true, false],
        'blind' => ['blind', false, false],
    ]);

    it('rejects an unknown safety mode with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'safety' => 'careless']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.safety');
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
        $compile = fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'wait_timeout' => $value]);

        if ($valid) {
            expect($compile()['native']['consumer']['wait_timeout'])->toBe($value);
        } else {
            expect($compile)->toThrow(InvalidArgumentException::class, 'queue.connections.orders.wait_timeout');
        }
    })->with([
        'below the minimum' => [999, false],
        'at the minimum' => [1000, true],
        'at the maximum' => [86_400_000, true],
        'beyond the maximum' => [86_400_001, false],
    ]);

    it('bounds prefetch between 1 and 65535', function (int $value, bool $valid): void {
        $compile = fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'prefetch' => $value]);

        if ($valid) {
            expect($compile()['native']['workers'][0]['subscriptions'][0]['prefetch'])->toBe($value);
        } else {
            expect($compile)->toThrow(InvalidArgumentException::class, 'queue.connections.orders.prefetch');
        }
    })->with([
        'zero' => [0, false],
        'at the maximum' => [65_535, true],
        'beyond the maximum' => [65_536, false],
    ]);

    it('bounds confirm_timeout to at least 1000', function (int $value, bool $valid): void {
        $compile = fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'confirm_timeout' => $value]);

        if ($valid) {
            expect($compile()['publisher']['confirm_timeout'])->toBe($value);
        } else {
            expect($compile)->toThrow(InvalidArgumentException::class, 'queue.connections.orders.confirm_timeout');
        }
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
    })->with([null, '', '   ', 'http://mq.local:15672']);

    it('rejects a non-string management_url with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'management_url' => 15672]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.management_url');
    });
});

describe('topology', function (): void {
    it('rejects delivery_limit without dead_letter', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'delivery_limit' => 20]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.dead_letter');
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
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'topology_mode' => 'managed']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.topology_mode');
    });

    it('rejects an unknown queue_type with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'queue_type' => 'lazy']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.queue_type');
    });
});

describe('credentials and tls', function (): void {
    it('maps explicit credentials and vhost', function (): void {
        $compiled = ConnectionCompiler::compile('orders', ['queue' => 'default', 'vhost' => '/eu', 'username' => 'app', 'password' => '']);

        expect($compiled['native']['brokers'][0]['vhost'])->toBe('/eu')
            ->and($compiled['native']['brokers'][0]['credentials'])->toBe(['username' => 'app', 'password' => '']);
    });

    it('allows an empty password but rejects an empty username', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'username' => '']))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.username');
    });

    it('rejects a non-string tls certificate path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'tls' => ['ca_cert' => 5]]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.tls.ca_cert');
    });
});

describe('delay', function (): void {
    it('rejects an unknown delay mode with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'delay' => ['mode' => 'cron']]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.delay.mode');
    });

    it('rejects empty delay buckets with the exact path', function (): void {
        expect(fn (): array => ConnectionCompiler::compile('orders', ['queue' => 'default', 'delay' => ['buckets' => []]]))
            ->toThrow(InvalidArgumentException::class, 'queue.connections.orders.delay.buckets');
    });
});

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
