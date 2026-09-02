<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Config;

use InvalidArgumentException;

/**
 * Compiles one queue.php connection into the native config shape expected by
 * the extension: one broker, one route, and one worker profile named after
 * the connection.
 */
final class ConnectionCompiler
{
    private const DEFAULT_AMQP_PORT = 5672;
    private const DEFAULT_CONSUMER_WAIT_TIMEOUT_MS = 30_000;
    private const MAX_CONSUMER_WAIT_TIMEOUT_MS = 86_400_000;
    private const DEFAULT_MAX_ATTEMPTS = 20;
    private const MSG_MUST_BE_ARRAY = 'must be an array';
    private const MSG_MUST_BE_NULL_OR_STRING = 'must be null or a string';

    /**
     * Top-level connection keys the compiler consumes. `driver` is read by
     * the queue dispatcher before compilation, not here; `after_commit` and
     * `block_for` are framework keys read by the connector from the raw
     * connection — listed so they ride through validation.
     */
    private const CONNECTION_KEYS = [
        'driver', 'queue', 'subscriptions', 'management_url',
        'hosts', 'vhost', 'username', 'password', 'tls', 'heartbeat',
        'exchange', 'routing_key',
        'safety', 'confirm_timeout',
        'prefetch', 'wait_timeout', 'max_attempts',
        'best_effort', 'auto_subscribe',
        'topology_mode',
        'queue_type', 'queue_durable', 'delivery_limit', 'dead_letter',
        'delay',
        'after_commit', 'block_for',
    ];

    /**
     * Sections whose package default merges per sub-key instead of
     * wholesale.
     */
    private const MERGED_SECTIONS = ['tls', 'delay', 'dead_letter'];

    /**
     * @param array<string, mixed> $config
     * @param array<string, mixed> $defaults package defaults merged under $config: every key the connection omits falls back to these (per sub-key for tls, delay, and dead_letter), and unknown top-level connection keys are only tolerated when a default covers them
     * @return array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array{safety: string, confirms: bool, mandatory: bool, confirm_timeout: int},
     *     topology: array<string, mixed>,
     *     best_effort: bool,
     *     auto_subscribe: bool
     * }
     */
    public static function compile(string $name, array $config, array $defaults = []): array
    {
        $path = 'queue.connections.'.$name;

        $config = self::mergeDefaults($config, $defaults);
        self::rejectUnknownKeys($config, array_merge(self::CONNECTION_KEYS, array_keys($defaults)), $path);

        $queue = self::string($config['queue'] ?? null, $path.'.queue');
        $broker = self::broker($name, $config, $path);
        $bestEffort = self::boolean($config['best_effort'] ?? false, $path.'.best_effort');
        $worker = self::worker($name, $queue, $config, $bestEffort, $path);
        $topology = self::topology($config, $path);
        $publisher = self::publisher($config, $path);
        $autoSubscribe = self::boolean($config['auto_subscribe'] ?? true, $path.'.auto_subscribe');

        return [
            'native' => [
                'brokers' => [$broker],
                'workers' => [$worker],
                'topology_mode' => self::topologyMode($config['topology_mode'] ?? 'declare', $path.'.topology_mode'),
                'delay' => self::delay($config['delay'] ?? [], $path.'.delay'),
                'dead_letter' => $topology['dead_letter'],
                'delivery_limit' => $topology['queue']['delivery_limit'],
                'publisher' => $publisher,
                'consumer' => self::consumer($config, $path),
                'queue_type' => $topology['queue']['type'],
                'queue_durable' => $topology['queue']['durable'],
            ],
            'routes' => [
                'default' => [
                    'broker' => $name,
                    'exchange' => self::exchange($config, $path),
                    'routing_key' => self::routingKey($config, $path),
                ],
            ],
            'publisher' => $publisher,
            'topology' => $topology,
            'best_effort' => $bestEffort,
            'auto_subscribe' => $autoSubscribe,
        ];
    }

    /**
     * Package defaults fill every key the connection omits; the connection
     * value — including an explicit null — always wins. Only the three known
     * nested sections merge per sub-key; every other key merges wholesale.
     * Keys unknown to the compiler may ride in through $defaults (e.g.
     * worker, production_warning) and are ignored downstream.
     *
     * @param array<string, mixed> $config
     * @param array<string, mixed> $defaults
     * @return array<string, mixed>
     */
    private static function mergeDefaults(array $config, array $defaults): array
    {
        foreach ($defaults as $key => $default) {
            if (! array_key_exists($key, $config)) {
                $config[$key] = $default;
                continue;
            }
            if (! in_array($key, self::MERGED_SECTIONS, true)
                || ! is_array($default)
                || ! is_array($config[$key])
            ) {
                continue;
            }
            foreach ($default as $subKey => $subDefault) {
                if (! array_key_exists($subKey, $config[$key])) {
                    $config[$key][$subKey] = $subDefault;
                }
            }
        }

        return $config;
    }

    /**
     * @param array<string, mixed> $config
     * @return array<string, mixed>
     */
    private static function broker(string $name, array $config, string $path): array
    {
        self::managementUrl($config['management_url'] ?? null, $path.'.management_url');

        $password = $config['password'] ?? 'guest';
        if (! is_string($password)) {
            self::invalid($path.'.password', 'must be a string');
        }

        return [
            'name' => $name,
            'hosts' => self::hosts($config['hosts'] ?? '127.0.0.1:5672', $path.'.hosts'),
            'vhost' => self::string($config['vhost'] ?? '/', $path.'.vhost'),
            'credentials' => [
                'username' => self::string($config['username'] ?? 'guest', $path.'.username'),
                'password' => $password,
            ],
            'tls' => self::tls($config['tls'] ?? [], $path.'.tls'),
            'heartbeat' => self::positiveInt($config['heartbeat'] ?? 30, $path.'.heartbeat'),
        ];
    }

    /**
     * Accepts a flat comma-separated string (env-friendly) or an array of
     * such strings. IPv6 endpoints must be bracketed: [::1]:5672.
     *
     * @return list<array{host: string, port: int}>
     */
    private static function hosts(mixed $hosts, string $path): array
    {
        if (is_string($hosts)) {
            $hosts = explode(',', $hosts);
        }

        if (! is_array($hosts) || $hosts === []) {
            self::invalid($path, 'must contain at least one host');
        }

        $endpoints = [];
        foreach ($hosts as $index => $host) {
            $endpoints[] = self::endpoint($host, $path.'.'.$index);
        }
        usort($endpoints, static fn (array $left, array $right): int => [
            $left['host'],
            $left['port'],
        ] <=> [
            $right['host'],
            $right['port'],
        ]);

        return $endpoints;
    }

    /**
     * @return array{host: string, port: int}
     */
    private static function endpoint(mixed $endpoint, string $path): array
    {
        if (! is_string($endpoint) || trim($endpoint) === '') {
            self::invalid($path, 'must be a non-empty host or host:port string');
        }

        $endpoint = trim($endpoint);
        $host = $endpoint;
        $port = self::DEFAULT_AMQP_PORT;

        if (str_starts_with($endpoint, '[')) {
            if (preg_match('/^\[([^]]+)](?::(\d+))?$/', $endpoint, $matches) !== 1) {
                self::invalid($path, 'contains an invalid bracketed IPv6 endpoint');
            }
            $host = $matches[1];
            $port = isset($matches[2]) ? (int) $matches[2] : self::DEFAULT_AMQP_PORT;
        } elseif (substr_count($endpoint, ':') === 1) {
            [$host, $rawPort] = explode(':', $endpoint, 2);
            if ($rawPort === '' || ! ctype_digit($rawPort)) {
                self::invalid($path, 'contains an invalid port');
            }
            $port = (int) $rawPort;
        }

        if ($host === '') {
            self::invalid($path, 'contains an empty host');
        }
        if ($port < 1 || $port > 65535) {
            self::invalid($path, 'port must be between 1 and 65535');
        }

        return ['host' => $host, 'port' => $port];
    }

    /**
     * @return array{enabled: bool, ca_cert: ?string, client_cert: ?string, client_key: ?string}
     */
    private static function tls(mixed $tls, string $path): array
    {
        if (! is_array($tls)) {
            self::invalid($path, self::MSG_MUST_BE_ARRAY);
        }
        self::rejectUnknownKeys($tls, ['enabled', 'ca_cert', 'client_cert', 'client_key'], $path);

        $caCert = $tls['ca_cert'] ?? null;
        if ($caCert !== null && ! is_string($caCert)) {
            self::invalid($path.'.ca_cert', self::MSG_MUST_BE_NULL_OR_STRING);
        }

        $clientCert = $tls['client_cert'] ?? null;
        if ($clientCert !== null && ! is_string($clientCert)) {
            self::invalid($path.'.client_cert', self::MSG_MUST_BE_NULL_OR_STRING);
        }

        $clientKey = $tls['client_key'] ?? null;
        if ($clientKey !== null && ! is_string($clientKey)) {
            self::invalid($path.'.client_key', self::MSG_MUST_BE_NULL_OR_STRING);
        }

        return [
            'enabled' => self::boolean($tls['enabled'] ?? false, $path.'.enabled'),
            'ca_cert' => $caCert,
            'client_cert' => $clientCert,
            'client_key' => $clientKey,
        ];
    }

    /**
     * Without the `subscriptions` key, one subscription named "default" is
     * derived from the connection's queue. With it, the escape-hatch list
     * replaces the derivation: the alias is the array key and the broker is
     * always this connection.
     *
     * @param array<string, mixed> $config
     * @return array{name: string, subscriptions: list<array<string, mixed>>, scheduler: array{strategy: string}}
     */
    private static function worker(string $name, string $queue, array $config, bool $bestEffort, string $path): array
    {
        $subscriptions = $config['subscriptions'] ?? null;

        if ($subscriptions === null) {
            return [
                'name' => $name,
                'subscriptions' => [
                    self::subscription($name, 'default', ['queue' => $queue], $config, $bestEffort, $path),
                ],
                'scheduler' => ['strategy' => 'weighted_fair'],
            ];
        }

        if (! is_array($subscriptions) || $subscriptions === []) {
            self::invalid($path.'.subscriptions', 'must contain at least one subscription');
        }

        $seenQueues = [];
        $compiled = [];
        foreach ($subscriptions as $alias => $entry) {
            $alias = (string) $alias;
            if ($alias === '') {
                self::invalid($path.'.subscriptions', 'subscription keys must be non-empty strings');
            }

            $subscriptionPath = $path.'.subscriptions.'.$alias;
            if (! is_array($entry)) {
                self::invalid($subscriptionPath, self::MSG_MUST_BE_ARRAY);
            }

            $queueName = self::string($entry['queue'] ?? null, $subscriptionPath.'.queue');
            if (isset($seenQueues[$queueName])) {
                self::invalid($subscriptionPath.'.queue', "duplicates the queue of subscription '{$seenQueues[$queueName]}'");
            }
            $seenQueues[$queueName] = $alias;

            $compiled[] = self::subscription($name, $alias, $entry, $config, $bestEffort, $subscriptionPath);
        }

        return [
            'name' => $name,
            'subscriptions' => $compiled,
            'scheduler' => ['strategy' => 'weighted_fair'],
        ];
    }

    /**
     * Ack flags follow the reliable-mode rules: early_ack and no_ack both
     * require best_effort, and no_ack additionally requires early_ack.
     *
     * @param array<string, mixed> $subscription
     * @param array<string, mixed> $config
     * @return array{name: string, broker: string, queue: string, weight: int, priority_class: int, prefetch: int, starvation_after: int, early_ack: bool, no_ack: bool}
     */
    private static function subscription(string $name, string $alias, array $subscription, array $config, bool $bestEffort, string $path): array
    {
        self::rejectUnknownKeys(
            $subscription,
            ['queue', 'weight', 'priority_class', 'prefetch', 'starvation_after', 'early_ack', 'no_ack'],
            $path,
        );

        $earlyAck = self::boolean($subscription['early_ack'] ?? false, $path.'.early_ack');
        if ($earlyAck && ! $bestEffort) {
            self::invalid($path.'.early_ack', 'early_ack is not allowed in reliable mode — set best_effort=true to opt in');
        }

        $noAck = self::boolean($subscription['no_ack'] ?? false, $path.'.no_ack');
        if ($noAck && ! $earlyAck) {
            self::invalid($path.'.no_ack', "no_ack=true requires early_ack=true for subscription '{$alias}'");
        }
        if ($noAck && ! $bestEffort) {
            self::invalid($path.'.no_ack', "no_ack=true requires best_effort=true for subscription '{$alias}'");
        }

        return [
            'name' => $alias,
            'broker' => $name,
            'queue' => self::string($subscription['queue'] ?? null, $path.'.queue'),
            'weight' => self::positiveInt($subscription['weight'] ?? 1, $path.'.weight', 65535),
            'priority_class' => self::boundedI16($subscription['priority_class'] ?? 0, $path.'.priority_class'),
            'prefetch' => self::positiveInt(
                $subscription['prefetch'] ?? ($config['prefetch'] ?? 64),
                $path.'.prefetch',
                65535,
            ),
            'starvation_after' => self::positiveInt($subscription['starvation_after'] ?? 30, $path.'.starvation_after'),
            'early_ack' => $earlyAck,
            'no_ack' => $noAck,
        ];
    }

    private static function boundedI16(mixed $value, string $path): int
    {
        $value = self::integer($value, $path);
        if ($value < -32768 || $value > 32767) {
            self::invalid($path, 'must be an integer between -32768 and 32767');
        }

        return $value;
    }

    /**
     * confirms and mandatory are derived from the safety mode, never set
     * independently: safe confirms and marks mandatory, unsafe confirms
     * without mandatory, blind does neither.
     *
     * @param array<string, mixed> $config
     * @return array{safety: string, confirms: bool, mandatory: bool, confirm_timeout: int}
     */
    private static function publisher(array $config, string $path): array
    {
        $safety = self::safetyMode($config['safety'] ?? 'safe', $path.'.safety');

        return [
            'safety' => $safety,
            'confirms' => $safety !== 'blind',
            'mandatory' => $safety === 'safe',
            'confirm_timeout' => self::confirmTimeout($config['confirm_timeout'] ?? 30_000, $path.'.confirm_timeout'),
        ];
    }

    private static function safetyMode(mixed $mode, string $path): string
    {
        if (! is_string($mode) || ! in_array($mode, ['safe', 'unsafe', 'blind'], true)) {
            self::invalid($path, 'must be safe, unsafe, or blind');
        }

        return $mode;
    }

    private static function confirmTimeout(mixed $value, string $path): int
    {
        $value = self::integer($value, $path);
        if ($value < 1000) {
            self::invalid($path, 'must be at least 1000');
        }

        return $value;
    }

    /**
     * @param array<string, mixed> $config
     * @return array{wait_timeout: int, max_attempts: int}
     */
    private static function consumer(array $config, string $path): array
    {
        $waitTimeout = self::integer($config['wait_timeout'] ?? self::DEFAULT_CONSUMER_WAIT_TIMEOUT_MS, $path.'.wait_timeout');
        if ($waitTimeout < 1000 || $waitTimeout > self::MAX_CONSUMER_WAIT_TIMEOUT_MS) {
            self::invalid($path.'.wait_timeout', 'must be between 1000 and '.self::MAX_CONSUMER_WAIT_TIMEOUT_MS);
        }

        return [
            'wait_timeout' => $waitTimeout,
            'max_attempts' => self::positiveInt($config['max_attempts'] ?? self::DEFAULT_MAX_ATTEMPTS, $path.'.max_attempts'),
        ];
    }

    /**
     * @return array{mode: string, buckets: list<int>, max_buckets: int, queue_expiry_margin: int}
     */
    private static function delay(mixed $delay, string $path): array
    {
        if (! is_array($delay)) {
            self::invalid($path, self::MSG_MUST_BE_ARRAY);
        }
        self::rejectUnknownKeys($delay, ['mode', 'buckets', 'max_buckets', 'queue_expiry_margin'], $path);

        $mode = $delay['mode'] ?? 'auto';
        if (! is_string($mode) || ! in_array($mode, ['auto', 'plugin', 'ttl'], true)) {
            self::invalid($path.'.mode', 'must be auto, plugin, or ttl');
        }

        $buckets = $delay['buckets'] ?? [1, 5, 30, 120];
        if (! is_array($buckets) || $buckets === []) {
            self::invalid($path.'.buckets', 'must contain at least one bucket');
        }
        $normalizedBuckets = [];
        foreach ($buckets as $index => $bucket) {
            $normalizedBuckets[] = self::positiveInt($bucket, $path.'.buckets.'.$index);
        }

        $maxBuckets = self::positiveInt($delay['max_buckets'] ?? 8, $path.'.max_buckets');
        if (count($normalizedBuckets) > $maxBuckets) {
            self::invalid($path.'.buckets', "bucket count exceeds configured maximum {$maxBuckets}");
        }

        return [
            'mode' => $mode,
            'buckets' => $normalizedBuckets,
            'max_buckets' => $maxBuckets,
            'queue_expiry_margin' => self::positiveInt($delay['queue_expiry_margin'] ?? 60, $path.'.queue_expiry_margin'),
        ];
    }

    /**
     * @param array<string, mixed> $config
     * @return array{queue: array{type: string, durable: bool, delivery_limit: ?int}, dead_letter: ?array<string, mixed>}
     */
    private static function topology(array $config, string $path): array
    {
        $type = $config['queue_type'] ?? 'quorum';
        if (! is_string($type) || ! in_array($type, ['quorum', 'classic'], true)) {
            self::invalid($path.'.queue_type', 'must be quorum or classic');
        }

        $deliveryLimit = $config['delivery_limit'] ?? null;
        if ($deliveryLimit !== null) {
            $deliveryLimit = self::positiveInt($deliveryLimit, $path.'.delivery_limit');
        }

        $deadLetter = self::deadLetter($config['dead_letter'] ?? null, $path.'.dead_letter');

        if ($deliveryLimit !== null && $deadLetter === null) {
            self::invalid(
                $path.'.dead_letter',
                'dead_letter must be configured when delivery_limit is set — '
                .'without it, poison messages are silently dropped by the quorum queue',
            );
        }

        return [
            'queue' => [
                'type' => $type,
                'durable' => self::boolean($config['queue_durable'] ?? true, $path.'.queue_durable'),
                'delivery_limit' => $deliveryLimit,
            ],
            'dead_letter' => $deadLetter,
        ];
    }

    /**
     * @return array<string, mixed>|null
     */
    private static function deadLetter(mixed $deadLetter, string $path): ?array
    {
        if ($deadLetter === null) {
            return null;
        }
        if (! is_array($deadLetter)) {
            self::invalid($path, 'must be null or an array');
        }
        self::rejectUnknownKeys($deadLetter, ['exchange', 'queue', 'routing_key'], $path);

        $routingKey = $deadLetter['routing_key'] ?? null;
        if ($routingKey !== null && (! is_string($routingKey) || $routingKey === '')) {
            self::invalid($path.'.routing_key', 'must be null or a non-empty string');
        }

        return [
            'enabled' => true,
            'exchange' => self::string($deadLetter['exchange'] ?? null, $path.'.exchange'),
            'queue' => self::string($deadLetter['queue'] ?? null, $path.'.queue'),
            'routing_key' => $routingKey,
        ];
    }

    /**
     * null publishes through the default exchange (direct-to-queue); an
     * explicit value must be a string (empty string is the default exchange).
     *
     * @param array<string, mixed> $config
     */
    private static function exchange(array $config, string $path): string
    {
        $exchange = array_key_exists('exchange', $config) ? $config['exchange'] : 'laravel.jobs';
        if ($exchange === null) {
            return '';
        }
        if (! is_string($exchange)) {
            self::invalid($path.'.exchange', 'must be a string or null');
        }

        return $exchange;
    }

    /**
     * null means "no routing key" (default-exchange and fanout usage).
     *
     * @param array<string, mixed> $config
     */
    private static function routingKey(array $config, string $path): string
    {
        $routingKey = array_key_exists('routing_key', $config) ? $config['routing_key'] : '{queue}';
        if ($routingKey === null) {
            return '';
        }
        if (! is_string($routingKey)) {
            self::invalid($path.'.routing_key', 'must be a string or null');
        }

        return $routingKey;
    }

    private static function topologyMode(mixed $mode, string $path): string
    {
        if (! is_string($mode) || ! in_array($mode, ['declare', 'verify', 'external'], true)) {
            self::invalid($path, 'must be declare, verify, or external');
        }

        return $mode;
    }

    /**
     * Management API base URL is a Laravel-only key (used by rabbit-rs:status,
     * not by the native extension): validated here, never propagated to native
     * config. Null or blank disables the feature.
     */
    private static function managementUrl(mixed $url, string $path): void
    {
        if ($url === null || (is_string($url) && trim($url) === '')) {
            return;
        }
        if (! is_string($url)) {
            self::invalid($path, self::MSG_MUST_BE_NULL_OR_STRING);
        }
    }

    private static function string(mixed $value, string $path, bool $allowEmpty = false): string
    {
        if (! is_string($value) || (! $allowEmpty && $value === '')) {
            self::invalid($path, $allowEmpty ? 'must be a string' : 'must be a non-empty string');
        }

        return $value;
    }

    private static function boolean(mixed $value, string $path): bool
    {
        if (is_bool($value)) {
            return $value;
        }

        // Laravel env() returns strings for .env flags (e.g. '1', 'true'),
        // so accept those forms and reject anything else strictly.
        if (is_string($value)) {
            $normalized = filter_var($value, FILTER_VALIDATE_BOOLEAN, FILTER_NULL_ON_FAILURE);
            if ($normalized !== null) {
                return $normalized;
            }
        }

        self::invalid($path, 'must be a boolean or an env-style boolean string (e.g. "1", "true")');
    }

    private static function integer(mixed $value, string $path): int
    {
        if (is_int($value)) {
            return $value;
        }

        // Laravel env() returns strings for .env numbers (e.g. '64'), so
        // accept signed integer strings and let the caller range-check.
        if (is_string($value) && preg_match('/^-?\d+$/', $value) === 1) {
            return (int) $value;
        }

        self::invalid($path, 'must be an integer or an env-style integer string (e.g. "64")');
    }

    private static function positiveInt(mixed $value, string $path, ?int $max = null): int
    {
        $value = self::integer($value, $path);
        if ($value < 1) {
            self::invalid($path, 'must be a positive integer');
        }
        if ($max !== null && $value > $max) {
            self::invalid($path, 'must be at most '.$max);
        }

        return $value;
    }

    /**
     * @param array<mixed> $section
     * @param list<string> $known
     */
    private static function rejectUnknownKeys(array $section, array $known, string $path): void
    {
        foreach (array_keys($section) as $key) {
            if (! in_array($key, $known, true)) {
                self::invalid($path.'.'.$key, 'unknown key');
            }
        }
    }

    private static function invalid(string $path, string $message): never
    {
        throw new InvalidArgumentException($path.': '.$message);
    }
}
