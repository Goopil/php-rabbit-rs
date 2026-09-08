<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Illuminate\Support\Arr;
use InvalidArgumentException;

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
     * Rabbit-rs connections targeted by an artisan --connection=* option
     * (comma-separated values allowed), in config order; all of them when
     * the option is absent.
     *
     * @param  list<string>  $names  raw option values
     * @return array<string, array<string, mixed>>
     */
    public static function targeted(array $names): array
    {
        $rabbitRs = self::all();

        $wanted = [];
        foreach ($names as $value) {
            foreach (explode(',', (string) $value) as $item) {
                $item = trim($item);
                if ($item !== '') {
                    $wanted[] = $item;
                }
            }
        }

        if ($wanted === []) {
            return $rabbitRs;
        }

        $unknown = array_values(array_unique(array_diff($wanted, array_keys($rabbitRs))));
        if ($unknown !== []) {
            throw new InvalidArgumentException(sprintf(
                'Unknown rabbit-rs connection(s): %s. Available rabbit-rs connections: %s',
                implode(', ', $unknown),
                implode(', ', array_keys($rabbitRs)),
            ));
        }

        return array_intersect_key($rabbitRs, array_flip($wanted));
    }

    /**
     * Package defaults as the service provider feeds them: the merged
     * `rabbit-rs` config minus the keys reserved for brokers, routes, and
     * workers (per sub-key for tls, delay and dead_letter).
     *
     * @return array<string, mixed>
     */
    public static function packageDefaults(): array
    {
        $config = config('rabbit-rs');

        return Arr::except(is_array($config) ? $config : [], ['brokers', 'routes', 'workers']);
    }

    /**
     * Queues a connection consumes: its `queue` key first, then every
     * `subscriptions.*.queue` not already listed.
     *
     * @param  array<string, mixed>  $config
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
