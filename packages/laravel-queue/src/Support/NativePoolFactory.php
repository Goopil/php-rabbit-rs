<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Closure;
use Goopil\RabbitRs\Laravel\Exceptions\PoolException;
use Goopil\RabbitRs\Pool;

final class NativePoolFactory
{
    /** @var array<string, Pool> */
    private array $pools = [];

    private int $processId;

    /** @var Closure(array<string, mixed>): Pool */
    private readonly Closure $createPool;

    /** @var Closure(): int */
    private readonly Closure $resolveProcessId;

    /**
     * @param (Closure(array<string, mixed>): Pool)|null $createPool
     * @param (Closure(): int)|null $resolveProcessId
     */
    public function __construct(
        ?Closure $createPool = null,
        ?Closure $resolveProcessId = null,
    ) {
        $this->createPool = $createPool ?? static fn (array $config): Pool => new Pool($config);
        $this->resolveProcessId = $resolveProcessId ?? static function (): int {
            $processId = getmypid();
            if ($processId === false) {
                throw new PoolException('Unable to determine the current process ID.');
            }

            return $processId;
        };
        $this->processId = ($this->resolveProcessId)();
    }

    /**
     * @param array<string, mixed> $nativeConfig
     */
    public function make(array $nativeConfig): Pool
    {
        $this->resetAfterFork();
        $fingerprint = hash('sha256', serialize($nativeConfig));

        return $this->pools[$fingerprint] ??= ($this->createPool)($nativeConfig);
    }

    /**
     * Clears all cached pools so the next make() creates fresh instances.
     *
     * Each cached pool is closed before being dropped so underlying AMQP
     * connections, channels, and file descriptors are released promptly.
     */
    public function flush(): void
    {
        $this->closePools();
        $this->pools = [];
        $this->processId = ($this->resolveProcessId)();
    }

    private function resetAfterFork(): void
    {
        $processId = ($this->resolveProcessId)();
        if ($processId === $this->processId) {
            return;
        }

        $this->closePools();
        $this->pools = [];
        $this->processId = $processId;
    }

    private function closePools(): void
    {
        foreach ($this->pools as $pool) {
            try {
                $pool->close();
            } catch (\Throwable) {
                // Best-effort close — the pool may already be disconnected.
            }
        }
    }
}
