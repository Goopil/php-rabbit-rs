<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;
use Illuminate\Container\Container;

final class OctaneLifecycleTest extends TestCase
{
    public function testTwoRequestsReuseTheSamePoolInOneWorker(): void
    {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = $this->normalizedNativeConfig();

        $pool1 = $factory->make($config);
        $pool2 = $factory->make($config);

        self::assertSame($pool1, $pool2, 'The same pool instance must be reused within one worker');
    }

    public function testNoRequestStateIsRetainedInPool(): void
    {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = $this->normalizedNativeConfig();

        $pool = $factory->make($config);
        $reflection = new \ReflectionClass($pool);
        $properties = array_map(fn (\ReflectionProperty $p): string => $p->getName(), $reflection->getProperties());

        self::assertNotContains('request', $properties);
        self::assertNotContains('requestId', $properties);
    }

    public function testOctaneLifecycleCanBeConstructedWithoutOctaneInstalled(): void
    {
        $lifecycle = new OctaneLifecycle($this->app);

        self::assertInstanceOf(OctaneLifecycle::class, $lifecycle);
    }

    public function testFlushDoesNotRecreateThePool(): void
    {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = $this->normalizedNativeConfig();

        $pool = $factory->make($config);
        self::assertSame($pool, $factory->make($config));

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->flush();

        $poolAfterFlush = $factory->make($config);
        self::assertSame($pool, $poolAfterFlush, 'Flush must not recreate the pool — no request state is retained');
    }

    public function testReloadClosesAllPools(): void
    {
        $factory = $this->app->make(NativePoolFactory::class);
        $config = $this->normalizedNativeConfig();

        $pool = $factory->make($config);
        self::assertSame($pool, $factory->make($config));

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->reload();

        $poolAfterReload = $factory->make($config);
        self::assertNotSame($pool, $poolAfterReload, 'Pool must be recreated after reload');
    }

    public function testWorkerStopDrainsPools(): void
    {
        $lifecycle = new OctaneLifecycle($this->app);

        $factory = $this->app->make(NativePoolFactory::class);
        $config = $this->normalizedNativeConfig();
        $pool = $factory->make($config);

        $lifecycle->stop();

        $poolAfterStop = $factory->make($config);
        self::assertNotSame($pool, $poolAfterStop, 'Pool must be recreated after worker stop');
    }

    public function testPoolIsIndependentPerWorker(): void
    {
        $factory1 = new NativePoolFactory();
        $factory2 = new NativePoolFactory();
        $config = $this->normalizedNativeConfig();

        $pool1 = $factory1->make($config);
        $pool2 = $factory2->make($config);

        self::assertNotSame($pool1, $pool2, 'Each worker must have its own pool instance');
    }

    public function testFlushClosesConsumersOnCurrentQueue(): void
    {
        [$queue, $pool] = $this->resolveQueueWithConsumer();
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->flush();

        self::assertSame(1, $consumer->closeCalls, 'flush() must close cached consumers');
    }

    public function testReloadClosesConsumersOnCurrentQueue(): void
    {
        [$queue, $pool] = $this->resolveQueueWithConsumer();
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->reload();

        self::assertSame(1, $consumer->closeCalls, 'reload() must close cached consumers');
    }

    public function testStopClosesConsumersOnCurrentQueue(): void
    {
        [$queue, $pool] = $this->resolveQueueWithConsumer();
        $consumer = $pool->consumerFor('default');

        $lifecycle = new OctaneLifecycle($this->app);
        $lifecycle->stop();

        self::assertSame(1, $consumer->closeCalls, 'stop() must close cached consumers');
    }

    public function testFlushWithoutQueueManagerDoesNotThrow(): void
    {
        $container = new Container();
        $lifecycle = new OctaneLifecycle($container);

        // Should not throw even though 'queue' is not bound.
        $lifecycle->flush();

        self::assertTrue(true);
    }

    /**
     * @return array{RabbitMqQueue, Pool}
     */
    private function resolveQueueWithConsumer(): array
    {
        $workers = [
            [
                'name' => 'default',
                'subscriptions' => [
                    ['name' => 'orders', 'queue' => 'orders-eu'],
                ],
            ],
        ];
        $routes = [
            'default' => [
                'broker' => 'default-broker',
                'exchange' => '',
                'routing_key' => '{queue}',
            ],
        ];

        $pool = new Pool(['workers' => $workers]);
        $resolver = new WorkerProfileResolver($workers);
        $queue = new RabbitMqQueue(
            $pool,
            $routes,
            'default',
            workerProfiles: $resolver,
            blockForMilliseconds: 0,
        );
        $queue->setContainer($this->app);
        $queue->setConnectionName('rabbit-rs');

        // Register the connection config so the manager knows about it.
        $this->app['config']->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
        ]);

        // Register the resolved connection so the manager returns our queue.
        $manager = $this->app->make('queue');
        $reflection = new \ReflectionClass($manager);
        $connectionsProperty = $reflection->getProperty('connections');
        $connectionsProperty->setValue($manager, ['rabbit-rs' => $queue]);

        // Trigger consumer creation by calling pop().
        $queue->pop('orders-eu');

        return [$queue, $pool];
    }

    /**
     * @return array<string, mixed>
     */
    private function normalizedNativeConfig(): array
    {
        $config = $this->app['config']->get('rabbit-rs');
        $normalized = \Goopil\RabbitRs\Laravel\Config\ConfigNormalizer::normalize(
            is_array($config) ? $config : [],
        );

        return $normalized['native'];
    }
}
