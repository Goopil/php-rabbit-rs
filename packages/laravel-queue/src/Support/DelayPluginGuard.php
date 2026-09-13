<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Goopil\RabbitRs\Laravel\Exceptions\DelayPluginMissingException;
use Illuminate\Support\Facades\Http;
use Illuminate\Support\Facades\Log;

/**
 * Broker-side presence check for the `rabbitmq_delayed_message_exchange`
 * plugin, read from the management API overview: `exchange_types` lists
 * `x-delayed-message` exactly when the plugin is enabled.
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
     * Drops every cached verdict (test isolation).
     */
    public static function reset(): void
    {
        self::$verdicts = [];
        self::$unverifiedWarnings = [];
    }

    /**
     * Effective delay mode for a compiled connection: `auto` keeps the plugin
     * strategy only when the broker confirms the delayed-message exchange
     * type, and degrades to the ttl bucket queues otherwise (plugin absent,
     * management API unreachable, or no management_url configured). Without
     * the degradation the native plugin strategy lands deferred jobs in the
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
     * A probe that cannot verify (no management_url, API down) publishes
     * through unchanged, so an unrelated management outage never breaks a
     * working plugin setup; the pass-through is logged once per connection.
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
                .'verified against the management API — delayed publishes are not guarded '
                .'against the missing rabbitmq_delayed_message_exchange plugin.',
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
        if (! is_string($url) || trim($url) === '') {
            return null;
        }

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
}
