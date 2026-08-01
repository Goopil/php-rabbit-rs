<?php

declare(strict_types=1);

namespace Goopil\RabbitRs {
    if (! class_exists(Pool::class, false)) {
        final class Pool
        {
            /**
             * @param array<string, mixed> $config
             */
            public function __construct(public readonly array $config) {}
        }
    }
}

namespace Goopil\RabbitRs\Laravel\Tests\Unit {
    use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
    use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
    use Goopil\RabbitRs\Laravel\RabbitMqQueue;
    use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
    use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
    use Goopil\RabbitRs\Laravel\Tests\TestCase;
    use Illuminate\Http\Request;
    use InvalidArgumentException;
    use ReflectionProperty;
    use WeakReference;

    final class RabbitMqConnectorTest extends TestCase
    {
        protected function setUp(): void
        {
            parent::setUp();

            $this->app['config']->set('queue.connections.rabbit-rs-primary', [
                'driver' => 'rabbit-rs',
                'queue' => 'default',
            ]);
            $this->app['config']->set('queue.connections.rabbit-rs-secondary', [
                'driver' => 'rabbit-rs',
                'queue' => 'secondary',
            ]);

            (new class($this->app) extends RabbitMqServiceProvider {
                protected function nativeExtensionLoaded(): bool
                {
                    return true;
                }
            })->boot();
        }

        public function testQueueManagerResolvesTheRabbitMqDriver(): void
        {
            $queue = $this->app['queue']->connection('rabbit-rs-primary');

            self::assertInstanceOf(RabbitMqQueue::class, $queue);
            self::assertSame('rabbit-rs-primary', $queue->getConnectionName());
        }

        public function testEquivalentQueueConnectionsShareThePoolWithoutSharingTheirProfile(): void
        {
            $primary = $this->app['queue']->connection('rabbit-rs-primary');
            $secondary = $this->app['queue']->connection('rabbit-rs-secondary');

            self::assertSame(
                $this->property($primary, 'pool'),
                $this->property($secondary, 'pool'),
            );
            self::assertSame('default', $this->property($primary, 'defaultQueue'));
            self::assertSame('secondary', $this->property($secondary, 'defaultQueue'));
        }

        public function testEquivalentNativeConfigurationsShareTheSamePool(): void
        {
            $factory = new NativePoolFactory();
            $config = $this->nativeConfig();

            self::assertSame($factory->make($config), $factory->make($config));
        }

        public function testDifferentNativeConfigurationsCreateDifferentPools(): void
        {
            $factory = new NativePoolFactory();
            $firstConfig = $this->nativeConfig();
            $secondConfig = $firstConfig;
            $secondConfig['brokers'][0]['heartbeat'] = 60;

            self::assertNotSame($factory->make($firstConfig), $factory->make($secondConfig));
        }

        public function testInheritedPoolsAreNotReusedAfterAFork(): void
        {
            $processId = 100;
            $factory = new NativePoolFactory(
                resolveProcessId: static function () use (&$processId): int {
                    return $processId;
                },
            );
            $config = $this->nativeConfig();
            $parentPool = $factory->make($config);

            $processId = 101;

            self::assertNotSame($parentPool, $factory->make($config));
        }

        public function testConnectorDoesNotRetainRequestScopedValues(): void
        {
            $request = new Request();
            $reference = WeakReference::create($request);
            $connector = new RabbitMqConnector(
                new NativePoolFactory(),
                ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs')),
            );

            $connector->connect([
                'driver' => 'rabbit-rs',
                'request' => $request,
            ]);
            unset($request);
            gc_collect_cycles();

            self::assertNull($reference->get());
        }

        public function testConnectorRejectsAnInvalidDefaultQueue(): void
        {
            $connector = new RabbitMqConnector(
                new NativePoolFactory(),
                ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs')),
            );

            $this->expectException(InvalidArgumentException::class);
            $this->expectExceptionMessage('queue');

            $connector->connect(['queue' => new Request()]);
        }

        /**
         * @return array<string, mixed>
         */
        private function nativeConfig(): array
        {
            return ConfigNormalizer::normalize(
                $this->app['config']->get('rabbit-rs'),
            )['native'];
        }

        private function property(object $object, string $name): mixed
        {
            return (new ReflectionProperty($object, $name))->getValue($object);
        }
    }
}