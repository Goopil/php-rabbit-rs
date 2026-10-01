<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Closure;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Exceptions\DelayPluginMissingException;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\Http;
use Illuminate\Support\Facades\Log;

/**
 * Broker-side presence check for the `rabbitmq_delayed_message_exchange`
 * plugin. Verdict resolution order per connection:
 *
 * 1. `management_url` set — the management API overview probe (primary,
 *    unchanged wire contract): `exchange_types` lists `x-delayed-message`
 *    exactly when the plugin is enabled.
 * 2. No `management_url` — native AMQP probe through the extension: a
 *    `rabbit-rs.probe.delayed` exchange declare succeeds only with the
 *    plugin (false = provably absent, null = extension missing or probe
 *    could not run).
 *
 * The verdict is cached per connection for the process lifetime — enabling
 * the plugin is a broker administration action, not something a running
 * process should re-probe for. A failed probe is cached too: without the
 * cache, an unreachable management API would turn every delayed publish
 * into a request-timeout pause.
 */
final class DelayPluginGuard
{
    private const DELAYED_EXCHANGE_TYPE = 'x-delayed-message';

    /** @var array<string, bool|null> connection name => plugin present (null: unverifiable) */
    private static array $verdicts = [];

    /** @var array<string, true> connections whose unverifiable pass-through was already logged */
    private static array $unverifiedWarnings = [];

    /**
     * Test seam: when set, it replaces the native AMQP probe for every
     * connection (`fn (string $connection): ?bool`). Cleared by `reset()`.
     */
    public static ?Closure $nativeProbe = null;

    /**
     * Drops every cached verdict (test isolation).
     */
    public static function reset(): void
    {
        self::$verdicts = [];
        self::$unverifiedWarnings = [];
        self::$nativeProbe = null;
    }

    /**
     * Effective delay mode for a compiled connection: `auto` keeps the plugin
     * strategy only when the broker confirms the delayed-message exchange
     * type, and degrades to the ttl bucket queues otherwise (plugin absent,
     * probe unverifiable, or no probe path configured). Without the
     * degradation the native plugin strategy lands deferred jobs in the
     * main queue until a sweep re-buckets them — an early-execution window.
     * Explicit plugin/ttl modes pass through untouched.
     */
    public static function resolveAutoMode(string $connection, string $mode): string
    {
        if ($mode !== 'auto') {
            return $mode;
        }

        return self::pluginPresent($connection) === true ? 'auto' : 'ttl';
    }

    /**
     * Refuses a delayed publish in plugin mode when the broker proves the
     * plugin absent — without it every deferred message is silently lost.
     * A probe that cannot verify publishes through unchanged, so an unrelated
     * probe-path outage never breaks a working plugin setup; the pass-through
     * is logged once per connection.
     */
    public static function assertPluginEnabled(string $connection): void
    {
        $present = self::pluginPresent($connection);

        if ($present === true) {
            return;
        }

        if ($present === false) {
            throw DelayPluginMissingException::forConnection($connection);
        }

        if (! isset(self::$unverifiedWarnings[$connection])) {
            self::$unverifiedWarnings[$connection] = true;
            Log::warning(
                "rabbit-rs: delay.mode=plugin on connection '{$connection}' could not be "
                .'verified against the broker (no management_url, management API unreachable, '
                .'or the rabbit_rs extension is unavailable) — delayed publishes are not '
                .'guarded against the missing rabbitmq_delayed_message_exchange plugin.',
            );
        }
    }

    /**
     * @return bool|null true: plugin enabled; false: plugin absent; null: unverifiable
     */
    private static function pluginPresent(string $connection): ?bool
    {
        if (array_key_exists($connection, self::$verdicts)) {
            return self::$verdicts[$connection];
        }

        // Mark the verdict in progress before probing: probeNative compiles
        // its throwaway pool through ConnectionCompiler::compile, which
        // re-enters this guard for the same connection. The re-entrant call
        // must observe the in-progress verdict (null → the nested compile
        // degrades to ttl) instead of probing again — writing the cache only
        // after the probe returned made compile → probe → compile recurse
        // until the process died (segfault under PCOV coverage, worker death
        // under Octane). The real verdict overwrites the marker below.
        self::$verdicts[$connection] = null;

        return self::$verdicts[$connection] = self::probeBroker($connection);
    }

    private static function probeBroker(string $connection): ?bool
    {
        // compile() runs in contexts without a booted container too (early
        // config validation); an unresolvable config helper means the broker
        // cannot be probed here, same verdict as an unreachable API.
        if (! app()->bound('config')) {
            return null;
        }

        $config = config('queue.connections.'.$connection);
        if (! is_array($config)) {
            return null;
        }

        $url = $config['management_url'] ?? null;
        if (is_string($url) && trim($url) !== '') {
            return self::probeManagementApi($config, $url);
        }

        return self::probeNative($connection, $config);
    }

    /**
     * @param  array<string, mixed>  $config
     */
    private static function probeManagementApi(array $config, string $url): ?bool
    {
        $username = $config['username'] ?? '';
        $password = $config['password'] ?? '';

        try {
            $response = Http::withBasicAuth(
                is_string($username) ? $username : '',
                is_string($password) ? $password : '',
            )
                ->timeout(5)
                ->acceptJson()
                ->get(rtrim(trim($url), '/').'/api/overview');
        } catch (\Throwable) {
            return null;
        }

        if (! $response->successful()) {
            return null;
        }

        $exchangeTypes = $response->json('exchange_types');
        if (! is_array($exchangeTypes)) {
            return null;
        }

        foreach ($exchangeTypes as $exchangeType) {
            if (is_array($exchangeType) && ($exchangeType['name'] ?? null) === self::DELAYED_EXCHANGE_TYPE) {
                return true;
            }
        }

        return false;
    }

    /**
     * @param  array<string, mixed>  $config
     */
    private static function probeNative(string $connection, array $config): ?bool
    {
        if (self::$nativeProbe !== null) {
            return (self::$nativeProbe)($connection);
        }

        if (! extension_loaded('rabbit_rs')) {
            return null;
        }

        try {
            // The probe pool is throwaway: pin its delay mode to ttl so this
            // compile never re-enters the guard for the plugin verdict the
            // probe itself is about to produce.
            $probeConfig = $config;
            $probeConfig['delay'] = array_merge(
                is_array($config['delay'] ?? null) ? $config['delay'] : [],
                ['mode' => 'ttl'],
            );

            $compiled = ConnectionCompiler::compile(
                $connection,
                $probeConfig,
                RabbitRsConnections::packageDefaults(),
            );
        } catch (\Throwable) {
            return null;
        }

        $broker = (string) ($compiled['native']['brokers'][0]['name'] ?? 'default');

        try {
            $pool = new Pool($compiled['native']);
            try {
                return $pool->probeDelayPlugin($broker);
            } finally {
                $pool->close();
            }
        } catch (\Throwable) {
            return null;
        }
    }
}
