<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

describe('flush', function (): void {
    it('closes all cached pools before clearing', function (): void {
        $pool = new Pool();

        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
        );

        $factory->make(['broker' => 'a']);

        expect($pool->closeCalls)->toBe(0)
            ->and($pool->stats()['closed'])->toBeFalse();

        $factory->flush();

        expect($pool->closeCalls)->toBe(1)
            ->and($pool->stats()['closed'])->toBeTrue();
    });

    it('closes every pool when multiple are cached', function (): void {
        $poolA = new Pool();
        $poolB = new Pool();

        $pools = ['a' => $poolA, 'b' => $poolB];
        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pools[$config['key']] ?? throw new TestException('unknown'),
        );

        $factory->make(['key' => 'a']);
        $factory->make(['key' => 'b']);

        $factory->flush();

        expect($poolA->closeCalls)->toBe(1)
            ->and($poolB->closeCalls)->toBe(1);
    });

    it('is safe to flush when no pools are cached', function (): void {
        $factory = new NativePoolFactory();

        $factory->flush();

        expect(true)->toBeTrue();
    });

    it('does not throw when a pool close raises', function (): void {
        $pool = new Pool();
        $pool->throwOnNextClose(new RuntimeException('already disconnected'));

        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
        );

        $factory->make(['key' => 'a']);

        $factory->flush();

        expect($pool->closeCalls)->toBe(1);
    });

    it('creates a fresh pool after flush', function (): void {
        $pool = new Pool();
        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
        );

        $factory->make(['key' => 'a']);
        $factory->flush();

        expect($pool->closeCalls)->toBe(1)
            ->and($pool->stats()['closed'])->toBeTrue();
    });
});

describe('resetAfterFork', function (): void {
    it('closes inherited pools when the process id changes', function (): void {
        $processId = 100;
        $pool = new Pool();

        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
            resolveProcessId: static function () use (&$processId): int {
                return $processId;
            },
        );

        $factory->make(['key' => 'a']);

        expect($pool->closeCalls)->toBe(0);

        $processId = 101;
        $factory->make(['key' => 'a']);

        expect($pool->closeCalls)->toBe(1)
            ->and($pool->stats()['closed'])->toBeTrue();
    });

    it('does not close pools when the process id is unchanged', function (): void {
        $processId = 100;
        $pool = new Pool();

        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
            resolveProcessId: static function () use (&$processId): int {
                return $processId;
            },
        );

        $factory->make(['key' => 'a']);
        $factory->make(['key' => 'a']);

        expect($pool->closeCalls)->toBe(0);
    });

    it('does not throw when an inherited pool close raises', function (): void {
        $processId = 100;
        $pool = new Pool();
        $pool->throwOnNextClose(new RuntimeException('already disconnected'));

        $factory = new NativePoolFactory(
            createPool: static fn (array $config): Pool => $pool,
            resolveProcessId: static function () use (&$processId): int {
                return $processId;
            },
        );

        $factory->make(['key' => 'a']);
        $processId = 101;

        $factory->make(['key' => 'a']);

        expect($pool->closeCalls)->toBe(1);
    });

    it('creates a fresh pool after fork detection', function (): void {
        $processId = 100;
        $parentPool = new Pool();
        $childPool = new Pool();
        $createCount = 0;

        $factory = new NativePoolFactory(
            createPool: static function (array $config) use (&$parentPool, &$childPool, &$createCount): Pool {
                $createCount++;

                return $createCount === 1 ? $parentPool : $childPool;
            },
            resolveProcessId: static function () use (&$processId): int {
                return $processId;
            },
        );

        $factory->make(['key' => 'a']);
        $processId = 101;
        $result = $factory->make(['key' => 'a']);

        expect($parentPool->closeCalls)->toBe(1)
            ->and($result)->toBe($childPool);
    });
});
