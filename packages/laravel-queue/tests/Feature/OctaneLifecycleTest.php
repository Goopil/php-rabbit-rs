<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Octane\OctaneLifecycle;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;

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
