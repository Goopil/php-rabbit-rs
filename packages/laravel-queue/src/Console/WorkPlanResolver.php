<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;

/**
 * Resolves the `rabbit-rs:work` fan-out plan from the queue connections
 * config.
 *
 * The plan maps every targeted connection to the queues it consumes:
 * - No flags: every `queue.connections.*` with driver `rabbit-rs`, in config
 *   order, consuming all its defined queues (`queue` key first, then
 *   `subscriptions.*.queue` values not already listed).
 * - `--connection=a,b`: filters the targeted connections (config order kept).
 * - `--queue=x,y`: each name is resolved BY DEFINITION on each targeted
 *   connection — a name matches the connection's `queue` key or one of its
 *   `subscriptions` alias keys (the alias resolves to that subscription's
 *   queue). Only targeted connections that define a listed queue consume it,
 *   so a queue defined on two targeted connections is consumed on both.
 */
final class WorkPlanResolver
{
    /**
     * @param string|null $connections Comma-separated connection names
     *         (null or empty: every rabbit-rs connection).
     * @param string|null $queues Comma-separated queue names resolved by
     *         definition (null or empty: every defined queue).
     *
     * @return list<array{connection: string, queues: list<string>}> one entry
     *         per targeted connection, in config order
     *
     * @throws \InvalidArgumentException when no rabbit-rs connection is
     *         configured, when a listed connection is unknown, or when a
     *         listed queue is not defined by any targeted connection
     */
    public static function resolve(?string $connections, ?string $queues): array
    {
        $rabbitRs = RabbitRsConnections::all();

        if ($rabbitRs === []) {
            throw new \InvalidArgumentException('No rabbit-rs queue connection is configured in queue.connections.');
        }

        $targeted = self::targetedConnections($connections, $rabbitRs);

        $queueNames = self::split($queues);

        if ($queueNames === []) {
            $plan = [];
            foreach ($targeted as $name => $config) {
                $plan[] = ['connection' => $name, 'queues' => RabbitRsConnections::definedQueues($config)];
            }

            return $plan;
        }

        $plan = [];
        $resolved = [];
        foreach ($targeted as $name => $config) {
            $queuesForConnection = [];
            foreach ($queueNames as $queueName) {
                $queue = self::definedQueueFor($config, $queueName);
                if ($queue === null) {
                    continue;
                }
                $resolved[$queueName] = true;
                if (! in_array($queue, $queuesForConnection, true)) {
                    $queuesForConnection[] = $queue;
                }
            }

            if ($queuesForConnection !== []) {
                $plan[] = ['connection' => $name, 'queues' => $queuesForConnection];
            }
        }

        $unknown = array_values(array_unique(array_diff($queueNames, array_keys($resolved))));
        if ($unknown !== []) {
            throw new \InvalidArgumentException(sprintf(
                'Unknown queue(s): %s. Defined queues: %s',
                implode(', ', $unknown),
                implode(', ', self::definedQueuesAcross($targeted)),
            ));
        }

        return $plan;
    }

    /**
     * Filter the rabbit-rs connections down to the listed names (config
     * order preserved, duplicates collapsed).
     *
     * @param array<string, array<string, mixed>> $rabbitRs
     * @return array<string, array<string, mixed>>
     */
    private static function targetedConnections(?string $connections, array $rabbitRs): array
    {
        $names = self::split($connections);

        if ($names === []) {
            return $rabbitRs;
        }

        $unknown = array_values(array_unique(array_diff($names, array_keys($rabbitRs))));
        if ($unknown !== []) {
            throw new \InvalidArgumentException(sprintf(
                'Unknown rabbit-rs connection(s): %s. Available rabbit-rs connections: %s',
                implode(', ', $unknown),
                implode(', ', array_keys($rabbitRs)),
            ));
        }

        return array_intersect_key($rabbitRs, array_flip($names));
    }

    /**
     * Resolve a listed name by definition on one connection: its `queue` key
     * or a `subscriptions` alias key (resolving to that subscription's
     * queue). Returns null when the connection does not define the name.
     *
     * @param array<string, mixed> $config
     */
    private static function definedQueueFor(array $config, string $name): ?string
    {
        $queue = $config['queue'] ?? null;
        if (is_string($queue) && $queue !== '' && $queue === $name) {
            return $queue;
        }

        foreach (self::subscriptions($config) as $alias => $subscription) {
            if (! is_array($subscription) || (string) $alias !== $name) {
                continue;
            }
            $subQueue = $subscription['queue'] ?? null;
            if (is_string($subQueue) && $subQueue !== '') {
                return $subQueue;
            }
        }

        return null;
    }

    /**
     * Union of the defined queue names across the given connections,
     * first-seen order preserved.
     *
     * @param array<string, array<string, mixed>> $configs
     * @return list<string>
     */
    private static function definedQueuesAcross(array $configs): array
    {
        $queues = [];
        foreach ($configs as $config) {
            foreach (RabbitRsConnections::definedQueues($config) as $queue) {
                if (! in_array($queue, $queues, true)) {
                    $queues[] = $queue;
                }
            }
        }

        return $queues;
    }

    /**
     * @param array<string, mixed> $config
     * @return array<string, mixed>
     */
    private static function subscriptions(array $config): array
    {
        $subscriptions = $config['subscriptions'] ?? [];

        return is_array($subscriptions) ? $subscriptions : [];
    }

    /**
     * Split a comma-separated flag value into trimmed, non-empty items.
     *
     * @return list<string>
     */
    private static function split(?string $value): array
    {
        if ($value === null || trim($value) === '') {
            return [];
        }

        return array_values(array_filter(
            array_map(static fn (string $item): string => trim($item), explode(',', $value)),
            static fn (string $item): bool => $item !== '',
        ));
    }
}
