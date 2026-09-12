<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Closure;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Pool;

/**
 * Per-connection ready-depth sampler for the rabbit-rs:work auto-scaler.
 *
 * Two sources, chosen per connection, summed over the connection's planned
 * queues:
 *
 * 1. The RabbitMQ management API (`messages_ready`) when the connection
 *    configures `management_url` — the zero-AMQP source, and the only one
 *    that still counts after every worker has exited (the one-shot final
 *    depth check).
 * 2. Otherwise a passive native probe (`Pool::size()`, the same passive
 *    declare the doctor and topology commands use): no management plugin
 *    is required, but while the sampler lives the supervisor holds one
 *    extra AMQP connection per connection, and each lookup blocks up to
 *    the connection's socket timeout.
 *
 * Every lookup degrades to null (request failure, unreachable broker,
 * missing queue, extension absent, config unusable): the caller silently
 * leaves the connection out of scaling, no warning — same contract as the
 * management API. A configured-but-failing management endpoint therefore
 * disables scaling for that connection rather than cascading into the
 * native probe. The supervisor never publishes, so its probe pools hold no
 * publish buffer and need no force-flush before size().
 *
 * Native probe pools are created lazily on first lookup and reused for the
 * sampler's lifetime; a failed lookup closes and drops the pool so the
 * next pass reconnects from scratch.
 */
final class QueueDepthSampler
{
    /** @var array<string, Pool> */
    private array $pools = [];

    /** @var array<string, array<string, mixed>|null> compiled native config per connection; null when unusable */
    private array $nativeConfigs = [];

    /**
     * @param  list<array{connection: string, queues: list<string>}>  $plan
     * @param  (Closure(string, string): int|null)|null  $nativeDepth  test seam:
     *                                                                 ready depth of one queue through the native path (connection
     *                                                                 name, queue name); when given, the real passive probe is
     *                                                                 bypassed entirely
     */
    public function __construct(
        private readonly array $plan,
        private readonly ?Closure $nativeDepth = null,
    ) {}

    /**
     * Ready depth per plan connection, null when none of its queues
     * reported a readable depth.
     *
     * @return array<string, int|null>
     */
    public function depths(): array
    {
        $depths = [];
        foreach ($this->plan as $entry) {
            $depths[$entry['connection']] = $this->depthFor($entry['connection'], $entry['queues']);
        }

        return $depths;
    }

    /**
     * Ready depth of one connection: the sum over its planned queues of the
     * first available source. Queues whose depth cannot be read contribute
     * nothing; a connection with no readable depth reports null.
     *
     * @param  list<string>  $queues
     */
    private function depthFor(string $connection, array $queues): ?int
    {
        $depth = 0;
        $known = false;
        foreach ($queues as $queue) {
            $queueDepth = $this->hasManagementUrl($connection)
                ? ManagementApi::queueDepth($connection, $queue)
                : $this->nativeQueueDepth($connection, $queue);
            if ($queueDepth !== null) {
                $known = true;
                $depth += $queueDepth;
            }
        }

        return $known ? $depth : null;
    }

    private function hasManagementUrl(string $connection): bool
    {
        $url = config('queue.connections.'.$connection.'.management_url');

        return is_string($url) && trim($url) !== '';
    }

    /**
     * Dispatches the native path: to the injected seam when present
     * (tests), otherwise to the real passive probe.
     */
    private function nativeQueueDepth(string $connection, string $queue): ?int
    {
        if ($this->nativeDepth !== null) {
            return ($this->nativeDepth)($connection, $queue);
        }

        return $this->probeQueueDepth($connection, $queue);
    }

    /**
     * Ready depth of one queue through a lazily created, cached native
     * pool. Any native failure (unreachable broker, auth, missing queue)
     * closes and drops the pool and reads as null: the next pass reconnects
     * from scratch instead of riding a broken socket.
     */
    private function probeQueueDepth(string $connection, string $queue): ?int
    {
        if (! extension_loaded('rabbit_rs')) {
            return null;
        }

        $nativeConfig = $this->nativeConfig($connection);
        if ($nativeConfig === null) {
            return null;
        }

        $pool = $this->pools[$connection] ??= new Pool($nativeConfig);

        try {
            return $pool->size(self::brokerName($nativeConfig), $queue);
        } catch (\Throwable) {
            $this->forgetPool($connection);

            return null;
        }
    }

    /**
     * Closes and drops the connection's probe pool, best-effort: the pool
     * may already be disconnected.
     */
    private function forgetPool(string $connection): void
    {
        $pool = $this->pools[$connection] ?? null;
        unset($this->pools[$connection]);
        if ($pool !== null) {
            try {
                $pool->close();
            } catch (\Throwable) {
                // Best-effort close — nothing else to do.
            }
        }
    }

    /**
     * Compiled native config of one connection, compiled once and cached.
     * Null when the connection is unknown or its compilation fails.
     *
     * @return array<string, mixed>|null
     */
    private function nativeConfig(string $connection): ?array
    {
        if (array_key_exists($connection, $this->nativeConfigs)) {
            return $this->nativeConfigs[$connection];
        }

        $config = config('queue.connections.'.$connection);
        if (! is_array($config)) {
            return $this->nativeConfigs[$connection] = null;
        }

        try {
            $compiled = ConnectionCompiler::compile($connection, $config, RabbitRsConnections::packageDefaults());
        } catch (\Throwable) {
            return $this->nativeConfigs[$connection] = null;
        }

        return $this->nativeConfigs[$connection] = $compiled['native'];
    }

    /**
     * The connection's first broker name, mirroring the doctor's probe
     * convention (today's compiler emits exactly one broker per connection).
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    private static function brokerName(array $nativeConfig): string
    {
        return $nativeConfig['brokers'][0]['name'] ?? 'default';
    }
}
