<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Illuminate\Support\ServiceProvider;
use InvalidArgumentException;
use PHPUnit\Framework\Attributes\DataProvider;
use RuntimeException;

final class ConfigNormalizerTest extends TestCase
{
    public function testPackagePublishesSafeDefaults(): void
    {
        $publishedConfig = require dirname(__DIR__, 2).'/config/rabbit-rs.php';

        self::assertSame('127.0.0.1:5672', $publishedConfig['brokers']['default']['hosts']);
        self::assertSame('declare', $publishedConfig['topology_mode']);
        self::assertTrue($publishedConfig['publisher']['confirms']);
        self::assertTrue($publishedConfig['publisher']['mandatory']);
        self::assertSame(30000, $publishedConfig['publisher']['confirm_timeout']);
        self::assertSame('quorum', $publishedConfig['topology']['queue']['type']);
        self::assertTrue($publishedConfig['topology']['queue']['durable']);
        self::assertSame(20, $publishedConfig['topology']['queue']['delivery_limit']);
        self::assertNull($publishedConfig['topology']['dead_letter']);

        $config = $this->app['config']->get('rabbit-rs');
        self::assertSame(['127.0.0.1:5672'], $config['brokers']['default']['hosts']);
        $normalized = ConfigNormalizer::normalize($config);
        self::assertSame('default', $normalized['routes']['default']['broker']);
        self::assertSame(16, $normalized['native']['workers'][0]['subscriptions'][0]['prefetch']);

        $paths = ServiceProvider::pathsToPublish(
            RabbitMqServiceProvider::class,
            'rabbit-rs-config',
        );

        self::assertSame(
            [dirname(__DIR__, 2).'/config/rabbit-rs.php' => config_path('rabbit-rs.php')],
            $paths,
        );
    }

    public function testNormalizesLaravelMapsForTheNativeExtension(): void
    {
        $normalized = ConfigNormalizer::normalize($this->validConfig());

        self::assertSame([
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
            'delivery_limit' => 20,
            'publisher' => [
                'confirms' => true,
                'mandatory' => true,
                'confirm_timeout' => 30000,
            ],
        ], $normalized['native']);
        self::assertSame('default', $normalized['routes']['orders']['broker']);
        self::assertTrue($normalized['publisher']['confirms']);
        self::assertTrue($normalized['publisher']['mandatory']);
        self::assertSame(30000, $normalized['publisher']['confirm_timeout']);
        self::assertNull($normalized['topology']['dead_letter']);
    }

    public function testPublisherSectionIsPropagatedToNativeConfig(): void
    {
        $config = $this->validConfig();
        $config['publisher'] = [
            'confirms' => false,
            'mandatory' => false,
            'confirm_timeout' => 5000,
        ];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame([
            'confirms' => false,
            'mandatory' => false,
            'confirm_timeout' => 5000,
        ], $normalized['native']['publisher']);
        self::assertFalse($normalized['publisher']['confirms']);
        self::assertFalse($normalized['publisher']['mandatory']);
        self::assertSame(5000, $normalized['publisher']['confirm_timeout']);
    }

    public function testPublisherConfirmTimeoutDefaultsToThirtySeconds(): void
    {
        $config = $this->validConfig();
        unset($config['publisher']['confirm_timeout']);

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame(30000, $normalized['native']['publisher']['confirm_timeout']);
        self::assertSame(30000, $normalized['publisher']['confirm_timeout']);
    }

    public function testRejectsNonPositivePublisherConfirmTimeout(): void
    {
        $config = $this->validConfig();
        $config['publisher']['confirm_timeout'] = 0;

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('publisher.confirm_timeout');

        ConfigNormalizer::normalize($config);
    }

    public function testNormalizesBracketedIpv6Endpoint(): void
    {
        $config = $this->validConfig();
        $config['brokers']['default']['hosts'] = ['[::1]:5673'];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame(
            [['host' => '::1', 'port' => 5673]],
            $normalized['native']['brokers'][0]['hosts'],
        );
    }

    /**
     * @param callable(array<string, mixed>): void $mutate
     */
    #[DataProvider('invalidConfigurations')]
    public function testRejectsInvalidConfigurationWithExactPath(
        callable $mutate,
        string $expectedPath,
    ): void {
        $config = $this->validConfig();
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
    }

    /**
     * @return iterable<string, array{callable(array<string, mixed>): void, string}>
     */
    public static function invalidConfigurations(): iterable
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
    }

    public function testNormalizesDeadLetterConfigIntoNativeConfig(): void
    {
        $config = $this->validConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => 'failed',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => 'failed',
        ], $normalized['native']['dead_letter']);
        self::assertSame(20, $normalized['native']['delivery_limit']);
        self::assertSame([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => 'failed',
        ], $normalized['topology']['dead_letter']);
    }

    public function testNullDeadLetterProducesNullInNativeConfig(): void
    {
        $config = $this->validConfig();

        $normalized = ConfigNormalizer::normalize($config);

        self::assertNull($normalized['native']['dead_letter']);
        self::assertSame(20, $normalized['native']['delivery_limit']);
    }

    public function testDeadLetterWithoutRoutingKeyProducesNullRoutingKey(): void
    {
        $config = $this->validConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame([
            'enabled' => true,
            'exchange' => 'orders.dlx',
            'queue' => 'orders.failed',
            'routing_key' => null,
        ], $normalized['native']['dead_letter']);
    }

    public function testRejectsDeadLetterWithoutExchange(): void
    {
        $config = $this->validConfig();
        $config['topology']['dead_letter'] = [
            'queue' => 'orders.failed',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('topology.dead_letter.exchange');

        ConfigNormalizer::normalize($config);
    }

    public function testRejectsDeadLetterWithoutQueue(): void
    {
        $config = $this->validConfig();
        $config['topology']['dead_letter'] = [
            'exchange' => 'orders.dlx',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('topology.dead_letter.queue');

        ConfigNormalizer::normalize($config);
    }

    public function testReportsMissingNativeExtensionExplicitly(): void
    {
        $provider = new class($this->app) extends RabbitMqServiceProvider {
            protected function nativeExtensionLoaded(): bool
            {
                return false;
            }
        };

        $this->expectException(RuntimeException::class);
        $this->expectExceptionMessage('ext-rabbit_rs');

        $provider->assertNativeExtensionLoaded();
    }

    public function testNormalizesTlsClientAndCaCertConfig(): void
    {
        $config = $this->validConfig();
        $config['brokers']['default']['tls'] = [
            'enabled' => true,
            'server_name' => 'broker.internal',
            'ca_cert' => '/etc/ssl/certs/ca.pem',
            'client_cert' => '/etc/ssl/client/cert.pem',
            'client_key' => '/etc/ssl/client/key.pem',
            'verify' => 'peer',
        ];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame([
            'enabled' => true,
            'server_name' => 'broker.internal',
            'ca_cert' => '/etc/ssl/certs/ca.pem',
            'client_cert' => '/etc/ssl/client/cert.pem',
            'client_key' => '/etc/ssl/client/key.pem',
            'verify' => 'peer',
        ], $normalized['native']['brokers'][0]['tls']);
    }

    public function testTlsDefaultsToPeerVerify(): void
    {
        $config = $this->validConfig();
        $config['brokers']['default']['tls'] = ['enabled' => true];

        $normalized = ConfigNormalizer::normalize($config);

        self::assertSame('peer', $normalized['native']['brokers'][0]['tls']['verify']);
    }

    public function testRejectsInvalidTlsVerifyMode(): void
    {
        $config = $this->validConfig();
        $config['brokers']['default']['tls'] = [
            'enabled' => true,
            'verify' => 'custom',
        ];

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('brokers.default.tls.verify');

        ConfigNormalizer::normalize($config);
    }

    /**
     * @return array<string, mixed>
     */
    private function validConfig(): array
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
                    'delivery_limit' => 20,
                ],
                'dead_letter' => null,
            ],
        ];
    }
}