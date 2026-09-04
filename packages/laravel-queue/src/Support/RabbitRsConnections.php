<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

/**
 * Listing of the queue connections driven by Rabbit RS and of the queues
 * each of them defines. Shared by the work plan resolver, the status
 * command, and the Octane lifecycle.
 */
final class RabbitRsConnections
{
    /**
     * Rabbit-rs queue connections from queue.connections, in config order.
     *
     * @return array<string, array<string, mixed>>
     */
    public static function all(): array
    {
        $connections = config('queue.connections');
        if (! is_array($connections)) {
            return [];
        }

        $rabbitRs = [];
        foreach ($connections as $name => $config) {
            if (is_array($config) && ($config['driver'] ?? null) === 'rabbit-rs') {
                $rabbitRs[(string) $name] = $config;
            }
        }

        return $rabbitRs;
    }

    /**
     * Queues a connection consumes: its `queue` key first, then every
     * `subscriptions.*.queue` not already listed.
     *
     * @param array<string, mixed> $config
     * @return list<string>
     */
    public static function definedQueues(array $config): array
    {
        $queues = [];

        $queue = $config['queue'] ?? null;
        if (is_string($queue) && $queue !== '') {
            $queues[] = $queue;
        }

        $subscriptions = $config['subscriptions'] ?? [];
        foreach (is_array($subscriptions) ? $subscriptions : [] as $subscription) {
            $subQueue = is_array($subscription) ? ($subscription['queue'] ?? null) : null;
            if (is_string($subQueue) && $subQueue !== '' && ! in_array($subQueue, $queues, true)) {
                $queues[] = $subQueue;
            }
        }

        return $queues;
    }
}
