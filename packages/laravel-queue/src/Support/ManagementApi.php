<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Illuminate\Support\Facades\Http;

/**
 * Cross-process counters from the RabbitMQ management API.
 *
 * Every lookup degrades to null when the connection has no
 * `management_url` or the request fails: callers silently fall back to
 * their non-API behavior (auto-scaling stays off, no warning is raised —
 * the API is optional and the driver itself never needs it).
 */
final class ManagementApi
{
    /**
     * Number of messages ready in one queue of the given connection, from
     * the management API (`GET {management_url}/api/queues/{vhost}/{queue}`,
     * top-level `messages_ready` gauge). Null without `management_url`, on
     * request failure, or when the response carries no gauge.
     */
    public static function queueDepth(string $connection, string $queue): ?int
    {
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
        $vhost = is_string($config['vhost'] ?? null) ? $config['vhost'] : '/';

        $endpoint = rtrim(trim($url), '/').'/api/queues/'.rawurlencode($vhost).'/'.rawurlencode($queue);

        try {
            $response = Http::withBasicAuth(
                is_string($username) ? $username : '',
                is_string($password) ? $password : '',
            )
                ->timeout(5)
                ->acceptJson()
                ->get($endpoint);
        } catch (\Throwable) {
            return null;
        }

        if (! $response->successful()) {
            return null;
        }

        $ready = $response->json('messages_ready');

        return is_numeric($ready) ? (int) $ready : null;
    }
}
