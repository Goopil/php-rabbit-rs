<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Closure;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Log;

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
 * missing queue, extension absent): the caller silently leaves the
 * connection out of scaling — same contract as the management API. A
 * configured-but-failing management endpoint therefore
 * disables scaling for that connection rather than cascading into the
 * native probe. The supervisor never publishes, so its probe pools hold no
 * publish buffer and need no force-flush before size().
 *
 * Native probe pools are created lazily on first lookup and reused for the
 * sampler's lifetime; a failed lookup closes and drops the pool so the
 * next pass reconnects from scratch.
 *
 * Native lookups (seam and real probe alike) are memoized per connection
 * and queue for {@see NATIVE_CACHE_TTL_SECONDS}: the supervisor samples on
 * every scale pass and once-mode drain check, so an unmemoized fallback
 * would pay one blocking AMQP round-trip per pass — and a slow or half-open
 * broker would turn the failure path's reconnect into a blocking retry
 * spin (issue #272). A failed probe is memoized for the TTL as well, so
 * the loop retries at most once per window; the management API path is
 * never cached (its HTTP client is already bounded).
 */
final class QueueDepthSampler
{
    /**
     * How long a native probe result (a depth or a failure) is reused
     * before the next blocking round-trip.
     */
    private const NATIVE_CACHE_TTL_SECONDS = 2.0;

    /** @var array<string, Pool> */
    private array $pools = [];

    /** @var array<string, array<string, mixed>|null> compiled native config per connection; null when unusable */
    private array $nativeConfigs = [];

    /** @var array<string, array{depth: int|null, expiresAt: float}> last native probe result per connection and queue, trusted until expiresAt */
    private array $nativeCache = [];

    /**
     * @param  list<array{connection: string, queues: list<string>}>  $plan
     * @param  (Closure(string, string): int|null)|null  $nativeDepth  test seam:
     *                                                                 ready depth of one queue through the native path (connection
     *                                                                 name, queue name); when given, the real passive probe is
     *                                                                 bypassed entirely
     * @param  float  $nativeCacheTtlSeconds  how long a native probe result is
     *                                        reused; tests pass 0 to force a probe on every lookup or a long
     *                                        TTL to pin one window
     */
    public function __construct(
        private readonly array $plan,
        private readonly ?Closure $nativeDepth = null,
        private readonly float $nativeCacheTtlSeconds = self::NATIVE_CACHE_TTL_SECONDS,
    ) {}

    /**
     * Ready depth per plan connection, null when none of its queues
     * reported a readable depth. Native lookups are memoized per
     * connection and queue for {@see NATIVE_CACHE_TTL_SECONDS}: pass
     * fresh: true to bypass the memoized read for a single call — the
     * supervisor's final once-mode drain check uses this so a stale 0 or
     * a memoized failed probe cannot end the drain with work pending
     * (issue #287).
     *
     * @return array<string, int|null>
     */
    public function depths(bool $fresh = false): array
    {
        $depths = [];
        foreach ($this->plan as $entry) {
            $depths[$entry['connection']] = $this->depthFor($entry['connection'], $entry['queues'], $fresh);
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
    private function depthFor(string $connection, array $queues, bool $fresh = false): ?int
    {
        $depth = 0;
        $known = false;
        foreach ($queues as $queue) {
            $queueDepth = $this->hasManagementUrl($connection)
                ? ManagementApi::queueDepth($connection, $queue)
                : $this->probeQueueDepth($connection, $queue, $fresh);
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
     * Ready depth of one queue through the native path, memoized for
     * {@see NATIVE_CACHE_TTL_SECONDS} (the injected seam when present —
     * wrapped by the same memo so tests exercise it — otherwise a lazily
     * created, cached passive-declare pool). Any native failure
     * (unreachable broker, auth, missing queue) closes and drops the pool
     * and reads as null: the next probe after the TTL reconnects from
     * scratch instead of riding a broken socket, and the memoized null
     * keeps the supervision loop from retrying sooner.
     */
    private function probeQueueDepth(string $connection, string $queue, bool $fresh = false): ?int
    {
        $cached = $this->nativeCache[$key = $connection.'|'.$queue] ?? null;
        if (! $fresh && $cached !== null && microtime(true) < $cached['expiresAt']) {
            return $cached['depth'];
        }

        $depth = $this->nativeDepth !== null
            ? ($this->nativeDepth)($connection, $queue)
            : $this->probeNativePool($connection, $queue);

        $this->nativeCache[$key] = [
            'depth' => $depth,
            'expiresAt' => microtime(true) + $this->nativeCacheTtlSeconds,
        ];

        return $depth;
    }

    /**
     * One blocking native round-trip for a queue's ready depth through the
     * connection's probe pool.
     */
    private function probeNativePool(string $connection, string $queue): ?int
    {
        // Compile check first: an unusable config is an operator error and
        // must warn (#310) even when the extension is absent (where the
        // probe would degrade to null anyway).
        $nativeConfig = $this->nativeConfig($connection);
        if ($nativeConfig === null) {
            return null;
        }

        if (! extension_loaded('rabbit_rs')) {
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
        } catch (\Throwable $e) {
            // A config that does not compile is an operator error, not
            // environmental noise (issue #310): say why the connection is
            // being dropped from scaling instead of degrading silently.
            Log::warning("rabbit-rs: connection [{$connection}] config is unusable for depth sampling, scaling skipped: {$e->getMessage()}");

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
