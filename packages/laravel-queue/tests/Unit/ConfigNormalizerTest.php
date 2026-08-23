<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Illuminate\Support\ServiceProvider;

describe('package defaults', function (): void {
    it('publishes safe defaults', function (): void {
        $publishedConfig = require dirname(__DIR__, 2).'/config/rabbit-rs.php';

        expect('127.0.0.1:5672')->toBe($publishedConfig['brokers']['default']['hosts'])
            ->and('declare')->toBe($publishedConfig['topology_mode'])
            ->and($publishedConfig['publisher']['confirms'])->toBeTrue()
            ->and($publishedConfig['publisher']['mandatory'])->toBeTrue()
            ->and(30000)->toBe($publishedConfig['publisher']['confirm_timeout'])
            ->and('quorum')->toBe($publishedConfig['topology']['queue']['type'])
            ->and($publishedConfig['topology']['queue']['durable'])->toBeTrue()
            ->and($publishedConfig['topology']['queue']['delivery_limit'])->toBeNull()
            ->and($publishedConfig['topology']['dead_letter'])->toBeNull();

        $config = $this->app['config']->get('rabbit-rs');
        expect(['127.0.0.1:5672'])->toBe($config['brokers']['default']['hosts']);
        $normalized = ConfigNormalizer::normalize($config);
        expect('default')->toBe($normalized['routes']['default']['broker'])
            ->and(64)->toBe($normalized['native']['workers'][0]['subscriptions'][0]['prefetch']);

        $paths = ServiceProvider::pathsToPublish(
            RabbitMqServiceProvider::class,
            'rabbit-rs-config',
        );

        expect(
            [dirname(__DIR__, 2).'/config/rabbit-rs.php' => config_path('rabbit-rs.php')],
        )->toBe($paths);
    });
});

describe('native normalization', function (): void {
    it('normalizes Laravel maps for the native extension', function (): void {
        $normalized = ConfigNormalizer::normalize(configValidConfig());

        expect([
            'brokers' => [[
                'name' => 'default',
                'hosts' => [
                    ['host' => 'rabbit-a', 'port' => 5672],
                    ['host' => 'rabbit-b', 'port' => 5673],
                ],
                'vhost' => '/',
                'credentials' => [
                    'username' => 'guest',
                    'password' => 'native-password-must-stay-secret',
                ],
                'tls' => [
                    'enabled' => false,
                    'server_name' => null,
                    'ca_cert' => null,
                    'client_cert' => null,
                    'client_key' => null,
                    'verify' => 'peer',
                ],
                'heartbeat' => 30,
            ]],
            'workers' => [[
                'name' => 'main',
                'subscriptions' => [[
                    'name' => 'orders',
                    'broker' => 'default',
                    'queue' => 'orders',
                    'weight' => 1,
                    'priority_class' => 0,
                    'prefetch' => 16,
                    'starvation_after' => 30,
                    'early_ack' => false,
                ]],
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
            ]],
            'topology_mode' => 'declare',
            'delay' => [
                'mode' => 'auto',
                'buckets' => [1, 5, 30, 120],
                'max_buckets' => 8,
                'queue_expiry_margin' => 60,
                'detection_timeout' => 5,
            ],
            'dead_letter' => null,
            'delivery_limit' => null,
            'publisher' => [
                'confirms' => true,
                'mandatory' => true,
                'confirm_timeout' => 30000,
            ],
            'queue_type' => 'quorum',
            'queue_durable' => true,
        ])->toBe($normalized['native'])
            ->and('default')->toBe($normalized['routes']['orders']['broker'])
            ->and($normalized['publisher']['confirms'])->toBeTrue()
            ->and($normalized['publisher']['mandatory'])->toBeTrue()
            ->and(30000)->toBe($normalized['publisher']['confirm_timeout'])
            ->and($normalized['topology']['dead_letter'])->toBeNull()
            ->and($normalized['best_effort'])->toBeFalse();
    });
});

describe('publisher section', function (): void {
    it('is propagated to native config', function (): void {
        $config = configValidConfig();
        $config['publisher'] = [
            'confirms' => false,
            'mandatory' => false,
            'confirm_timeout' => 5000,
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect([
            'confirms' => false,
            'mandatory' => false,
            'confirm_timeout' => 5000,
        ])->toBe($normalized['native']['publisher'])
            ->and($normalized['publisher']['confirms'])->toBeFalse()
            ->and($normalized['publisher']['mandatory'])->toBeFalse()
            ->and(5000)->toBe($normalized['publisher']['confirm_timeout']);
    });

    it('defaults the confirm timeout to thirty seconds', function (): void {
        $config = configValidConfig();
        unset($config['publisher']['confirm_timeout']);

        $normalized = ConfigNormalizer::normalize($config);

        expect(30000)->toBe($normalized['native']['publisher']['confirm_timeout'])
            ->and(30000)->toBe($normalized['publisher']['confirm_timeout']);
    });

    it('rejects a non-positive confirm timeout', function (): void {
        $config = configValidConfig();
        $config['publisher']['confirm_timeout'] = 0;

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('publisher.confirm_timeout');

        ConfigNormalizer::normalize($config);
    });
});

describe('IPv6', function (): void {
    it('normalizes a bracketed IPv6 endpoint', function (): void {
        $config = configValidConfig();
        $config['brokers']['default']['hosts'] = ['[::1]:5673'];

        $normalized = ConfigNormalizer::normalize($config);

        expect(
            [['host' => '::1', 'port' => 5673]],
        )->toBe($normalized['native']['brokers'][0]['hosts']);
    });
});

describe('invalid configuration', function (): void {
    it('rejects invalid configuration with the exact path', function (callable $mutate, string $expectedPath): void {
        $config = configValidConfig();
        $mutate($config);

        try {
            ConfigNormalizer::normalize($config);
            self::fail('Invalid configuration should be rejected.');
        } catch (InvalidArgumentException $exception) {
            self::assertStringContainsString($expectedPath, $exception->getMessage());
            self::assertStringNotContainsString(
                'native-password-must-stay-secret',
                $exception->getMessage(),
            );
        }
    })->with(configInvalidConfigurations());
});

describe('dead letter', function (): void {
    it('normalizes dead letter config into native config', function (): void {
        $config = configValidConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => 'failed',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => 'failed',
        ])->toBe($normalized['native']['dead_letter'])
            ->and($normalized['native']['delivery_limit'])->toBeNull()
            ->and([
                'enabled' => true,
                'exchange' => 'orders.dlx',
                'queue' => 'orders.failed',
                'routing_key' => 'failed',
            ])->toBe($normalized['topology']['dead_letter']);
    });

    it('produces null in native config when dead letter is null', function (): void {
        $config = configValidConfig();

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized['native']['dead_letter'])->toBeNull()
            ->and($normalized['native']['delivery_limit'])->toBeNull();
    });

    it('produces a null routing key when dead letter has no routing key', function (): void {
        $config = configValidConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => null,
        ])->toBe($normalized['native']['dead_letter']);
    });

    it('rejects a dead letter without an exchange', function (): void {
        $config = configValidConfig();
        $config['topology']['dead_letter'] = [
            'queue' => 'orders.failed',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('topology.dead_letter.exchange');

        ConfigNormalizer::normalize($config);
    });

    it('rejects a dead letter without a queue', function (): void {
        $config = configValidConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('topology.dead_letter.queue');

        ConfigNormalizer::normalize($config);
    });
});

describe('delivery_limit DLX guard', function (): void {
    it('rejects delivery_limit without dead_letter', function (): void {
        $config = configValidConfig();
        $config['topology']['queue']['delivery_limit'] = 20;
        $config['topology']['dead_letter'] = null;

        expect(fn (): array => ConfigNormalizer::normalize($config))
            ->toThrow(InvalidArgumentException::class, 'dead_letter must be configured');
    });

    it('accepts delivery_limit with dead_letter', function (): void {
        $config = configValidConfig();
        $config['topology']['queue']['delivery_limit'] = 20;
        $config['topology']['dead_letter'] = ['exchange' => 'dlx', 'queue' => 'dlq'];

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized)->toBeArray()
            ->and(20)->toBe($normalized['native']['delivery_limit']);
    });

    it('accepts null delivery_limit with null dead_letter', function (): void {
        $config = configValidConfig();
        $config['topology']['queue']['delivery_limit'] = null;
        $config['topology']['dead_letter'] = null;

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized)->toBeArray()
            ->and($normalized['native']['delivery_limit'])->toBeNull();
    });
});

describe('queue type and durable', function (): void {
    it('passes queue type to native config', function (): void {
        $config = configValidConfig();
        $config['topology']['queue']['type'] = 'classic';

        $normalized = ConfigNormalizer::normalize($config);

        expect('classic')->toBe($normalized['native']['queue_type']);
    });

    it('passes queue durable to native config', function (): void {
        $config = configValidConfig();
        $config['topology']['queue']['durable'] = false;

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized['native']['queue_durable'])->toBeFalse();
    });

    it('defaults queue type to quorum and durable to true', function (): void {
        $config = configValidConfig();

        $normalized = ConfigNormalizer::normalize($config);

        expect('quorum')->toBe($normalized['native']['queue_type'])
            ->and($normalized['native']['queue_durable'])->toBeTrue();
    });
});

describe('early_ack guard', function (): void {
    it('rejects early_ack in reliable mode', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['early_ack'] = true;

        expect(fn (): array => ConfigNormalizer::normalize($config))
            ->toThrow(InvalidArgumentException::class, 'early_ack');
    });

    it('allows early_ack when best_effort is true', function (): void {
        $config = configValidConfig();
        $config['best_effort'] = true;
        $config['workers']['main']['subscriptions']['orders']['early_ack'] = true;

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized['native']['workers'][0]['subscriptions'][0]['early_ack'])->toBeTrue()
            ->and($normalized['best_effort'])->toBeTrue();
    });

    it('defaults early_ack to false', function (): void {
        $config = configValidConfig();

        $normalized = ConfigNormalizer::normalize($config);

        expect($normalized['native']['workers'][0]['subscriptions'][0]['early_ack'])->toBeFalse();
    });
});

describe('native extension', function (): void {
    it('reports a missing native extension explicitly', function (): void {
        $provider = new class($this->app) extends RabbitMqServiceProvider {
            protected function nativeExtensionLoaded(): bool
            {
                return false;
            }
        };

        $this->expectException(RuntimeException::class);
        $this->expectExceptionMessage('ext-rabbit_rs');

        $provider->assertNativeExtensionLoaded();
    });
});

describe('TLS', function (): void {
    it('normalizes TLS client and CA cert config', function (): void {
        $config = configValidConfig();
        $config['brokers']['default']['tls'] = [
            'enabled' => true,
            'server_name' => 'broker.internal',
            'ca_cert' => '/etc/ssl/certs/ca.pem',
            'client_cert' => '/etc/ssl/client/cert.pem',
            'client_key' => '/etc/ssl/client/key.pem',
            'verify' => 'peer',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect([
            'enabled' => true,
            'server_name' => 'broker.internal',
            'ca_cert' => '/etc/ssl/certs/ca.pem',
            'client_cert' => '/etc/ssl/client/cert.pem',
            'client_key' => '/etc/ssl/client/key.pem',
            'verify' => 'peer',
        ])->toBe($normalized['native']['brokers'][0]['tls']);
    });

    it('defaults TLS verify to peer', function (): void {
        $config = configValidConfig();
        $config['brokers']['default']['tls'] = ['enabled' => true];

        $normalized = ConfigNormalizer::normalize($config);

        expect('peer')->toBe($normalized['native']['brokers'][0]['tls']['verify']);
    });

    it('rejects an invalid TLS verify mode', function (): void {
        $config = configValidConfig();
        $config['brokers']['default']['tls'] = [
            'enabled' => true,
            'verify' => 'custom',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('brokers.default.tls.verify');

        ConfigNormalizer::normalize($config);
    });
});

/**
 * @return array<string, mixed>
 */
function configValidConfig(): array
{
    return [
        'topology_mode' => 'declare',
        'brokers' => [
            'default' => [
                'hosts' => ['rabbit-b:5673', 'rabbit-a'],
                'vhost' => '/',
                'credentials' => [
                    'username' => 'guest',
                    'password' => 'native-password-must-stay-secret',
                ],
                'tls' => [
                    'enabled' => false,
                    'server_name' => null,
                    'ca_cert' => null,
                    'client_cert' => null,
                    'client_key' => null,
                    'verify' => 'peer',
                ],
                'heartbeat' => 30,
            ],
        ],
        'routes' => [
            'orders' => [
                'broker' => 'default',
                'exchange' => 'laravel.jobs',
                'routing_key' => '{queue}',
            ],
        ],
        'workers' => [
            'main' => [
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
                'subscriptions' => [
                    'orders' => [
                        'broker' => 'default',
                        'queue' => 'orders',
                        'prefetch' => ['mode' => 'fixed', 'value' => 16],
                    ],
                ],
            ],
        ],
        'publisher' => ['confirms' => true, 'mandatory' => true],
        'topology' => [
            'queue' => [
                'type' => 'quorum',
                'durable' => true,
                'delivery_limit' => null,
            ],
            'dead_letter' => null,
        ],
    ];
}

/**
 * @return iterable<string, array{callable(array<string, mixed>): void, string}>
 */
function configInvalidConfigurations(): iterable
{
    yield 'missing brokers' => [
        static function (array &$config): void {
            $config['brokers'] = [];
        },
        'brokers',
    ];
    yield 'broker without hosts' => [
        static function (array &$config): void {
            $config['brokers']['default']['hosts'] = [];
        },
        'brokers.default.hosts',
    ];
    yield 'route with unknown broker' => [
        static function (array &$config): void {
            $config['routes']['orders']['broker'] = 'missing';
        },
        'routes.orders.broker',
    ];
    yield 'worker with unknown broker' => [
        static function (array &$config): void {
            $config['workers']['main']['subscriptions']['orders']['broker'] = 'missing';
        },
        'workers.main.subscriptions.orders.broker',
    ];
    yield 'zero prefetch' => [
        static function (array &$config): void {
            $config['workers']['main']['subscriptions']['orders']['prefetch']['value'] = 0;
        },
        'workers.main.subscriptions.orders.prefetch.value',
    ];
    yield 'prefetch above worker budget' => [
        static function (array &$config): void {
            $config['workers']['main']['scheduler']['max_in_flight'] = 8;
        },
        'workers.main.scheduler.max_in_flight',
    ];
    yield 'zero starvation duration' => [
        static function (array &$config): void {
            $config['workers']['main']['subscriptions']['orders']['starvation_after'] = 0;
        },
        'workers.main.subscriptions.orders.starvation_after',
    ];
    yield 'unsupported topology mode' => [
        static function (array &$config): void {
            $config['topology_mode'] = 'managed';
        },
        'topology_mode',
    ];
    yield 'early_ack in reliable mode' => [
        static function (array &$config): void {
            $config['workers']['main']['subscriptions']['orders']['early_ack'] = true;
        },
        'workers.main.subscriptions.orders.early_ack',
    ];
    yield 'delivery_limit without dead_letter' => [
        static function (array &$config): void {
            $config['topology']['queue']['delivery_limit'] = 20;
        },
        'topology.dead_letter',
    ];
}
