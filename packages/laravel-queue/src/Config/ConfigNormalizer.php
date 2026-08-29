<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Config;

use InvalidArgumentException;

final class ConfigNormalizer
{
    private const DEFAULT_AMQP_PORT = 5672;
    private const MSG_MUST_BE_ARRAY = 'must be an array';
    private const MSG_MUST_BE_NULL_OR_STRING = 'must be null or a string';
    private const MSG_NO_ACK = '.no_ack';
    private const MSG_BROKER = '.broker';
    private const MSG_SUBSCRIPTIONS = '.subscriptions';

    /**
     * @param array<string, mixed> $config
     * @return array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array{confirms: bool, mandatory: bool, confirm_timeout: int},
     *     topology: array<string, mixed>
     * }
     */
    public static function normalize(array $config): array
    {
        $topologyMode = self::topologyMode($config['topology_mode'] ?? 'declare');
        $brokers = self::brokers($config['brokers'] ?? null);
        $brokerNames = array_fill_keys(array_column($brokers, 'name'), true);
        $topology = self::topology($config['topology'] ?? []);
        $publisher = self::publisher($config['publisher'] ?? []);
        $bestEffort = self::boolean($config['best_effort'] ?? false, 'best_effort');

        return [
            'native' => [
                'brokers' => $brokers,
                'workers' => self::workers($config['workers'] ?? [], $brokerNames, $bestEffort),
                'topology_mode' => $topologyMode,
                'delay' => self::delay($config['delay'] ?? []),
                'dead_letter' => $topology['dead_letter'],
                'delivery_limit' => $topology['queue']['delivery_limit'],
                'publisher' => $publisher,
                'queue_type' => $topology['queue']['type'],
                'queue_durable' => $topology['queue']['durable'],
            ],
            'routes' => self::routes($config['routes'] ?? [], $brokerNames),
            'publisher' => $publisher,
            'topology' => $topology,
            'best_effort' => $bestEffort,
        ];
    }

    private static function topologyMode(mixed $mode): string
    {
        if (! is_string($mode) || ! in_array($mode, ['declare', 'verify', 'external'], true)) {
            self::invalid('topology_mode', 'must be declare, verify, or external');
        }

        return $mode;
    }

    /**
     * @return list<array<string, mixed>>
     */
    private static function brokers(mixed $brokers): array
    {
        if (! is_array($brokers) || $brokers === []) {
            self::invalid('brokers', 'must contain at least one broker');
        }

        ksort($brokers);
        $normalized = [];

        foreach ($brokers as $name => $broker) {
            $path = 'brokers.'.self::name($name, 'brokers');
            if (! is_array($broker)) {
                self::invalid($path, self::MSG_MUST_BE_ARRAY);
            }

            $hosts = $broker['hosts'] ?? null;
            if (! is_array($hosts) || $hosts === []) {
                self::invalid($path.'.hosts', 'must contain at least one host');
            }

            $endpoints = [];
            foreach ($hosts as $index => $host) {
                $endpoints[] = self::endpoint($host, $path.'.hosts.'.$index);
            }
            usort($endpoints, static fn (array $left, array $right): int => [
                $left['host'],
                $left['port'],
            ] <=> [
                $right['host'],
                $right['port'],
            ]);

            $credentials = $broker['credentials'] ?? null;
            if (! is_array($credentials)) {
                self::invalid($path.'.credentials', self::MSG_MUST_BE_ARRAY);
            }

            $username = self::string($credentials['username'] ?? null, $path.'.credentials.username');
            $password = self::string($credentials['password'] ?? null, $path.'.credentials.password', true);
            $tls = self::tls($broker['tls'] ?? [], $path.'.tls');

            $normalized[] = [
                'name' => (string) $name,
                'hosts' => $endpoints,
                'vhost' => self::string($broker['vhost'] ?? '/', $path.'.vhost'),
                'credentials' => ['username' => $username, 'password' => $password],
                'tls' => $tls,
                'heartbeat' => self::positiveInt($broker['heartbeat'] ?? 30, $path.'.heartbeat'),
            ];
        }

        return $normalized;
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
     * @return array{enabled: bool, server_name: ?string, ca_cert: ?string, client_cert: ?string, client_key: ?string, verify: string}
     */
    private static function tls(mixed $tls, string $path): array
    {
        if (! is_array($tls)) {
            self::invalid($path, self::MSG_MUST_BE_ARRAY);
        }

        $enabled = $tls['enabled'] ?? false;
        if (! is_bool($enabled)) {
            self::invalid($path.'.enabled', 'must be a boolean');
        }

        $serverName = $tls['server_name'] ?? null;
        if ($serverName !== null && (! is_string($serverName) || $serverName === '')) {
            self::invalid($path.'.server_name', 'must be null or a non-empty string');
        }

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

        $verify = $tls['verify'] ?? 'peer';
        if (! is_string($verify) || ! in_array($verify, ['peer', 'none'], true)) {
            self::invalid($path.'.verify', 'must be peer or none');
        }

        return [
            'enabled' => $enabled,
            'server_name' => $serverName,
            'ca_cert' => $caCert,
            'client_cert' => $clientCert,
            'client_key' => $clientKey,
            'verify' => $verify,
        ];
    }

    /**
     * @param array<string, true> $brokerNames
     * @return array<string, array<string, mixed>>
     */
    private static function routes(mixed $routes, array $brokerNames): array
    {
        if (! is_array($routes)) {
            self::invalid('routes', self::MSG_MUST_BE_ARRAY);
        }

        ksort($routes);
        $normalized = [];
        foreach ($routes as $name => $route) {
            $path = 'routes.'.self::name($name, 'routes');
            if (! is_array($route)) {
                self::invalid($path, self::MSG_MUST_BE_ARRAY);
            }

            $broker = self::string($route['broker'] ?? null, $path.self::MSG_BROKER);
            if (! isset($brokerNames[$broker])) {
                self::invalid($path.self::MSG_BROKER, 'references an unknown broker');
            }

            $normalized[(string) $name] = [
                'broker' => $broker,
                'exchange' => self::string($route['exchange'] ?? null, $path.'.exchange', true),
                'routing_key' => self::string($route['routing_key'] ?? null, $path.'.routing_key', true),
            ];
        }

        return $normalized;
    }

    /**
     * @param array<string, true> $brokerNames
     * @return list<array<string, mixed>>
     */
    private static function workers(mixed $workers, array $brokerNames, bool $bestEffort): array
    {
        if (! is_array($workers)) {
            self::invalid('workers', self::MSG_MUST_BE_ARRAY);
        }

        ksort($workers);
        $normalized = [];
        foreach ($workers as $name => $worker) {
            $path = 'workers.'.self::name($name, 'workers');

            $normalized[] = self::normalizeWorker($worker, (string) $name, $path, $brokerNames, $bestEffort);
        }

        return $normalized;
    }

    /**
     * @param array<string, true> $brokerNames
     * @return array<string, mixed>
     */
    private static function normalizeWorker(
        mixed $worker,
        string $workerName,
        string $path,
        array $brokerNames,
        bool $bestEffort,
    ): array {
        if (! is_array($worker)) {
            self::invalid($path, self::MSG_MUST_BE_ARRAY);
        }

        $scheduler = $worker['scheduler'] ?? null;
        if (! is_array($scheduler)) {
            self::invalid($path.'.scheduler', self::MSG_MUST_BE_ARRAY);
        }
        if (($scheduler['strategy'] ?? 'weighted_fair') !== 'weighted_fair') {
            self::invalid($path.'.scheduler.strategy', 'must be weighted_fair');
        }

        $subscriptions = $worker['subscriptions'] ?? null;
        if (! is_array($subscriptions) || $subscriptions === []) {
            self::invalid($path.self::MSG_SUBSCRIPTIONS, 'must contain at least one subscription');
        }
        ksort($subscriptions);

        $normalizedSubscriptions = [];
        foreach ($subscriptions as $subscriptionName => $subscription) {
            $subscriptionPath = $path.'.subscriptions.'.self::name(
                $subscriptionName,
                $path.self::MSG_SUBSCRIPTIONS,
            );
            if (! is_array($subscription)) {
                self::invalid($subscriptionPath, self::MSG_MUST_BE_ARRAY);
            }
            if (! self::boolean($subscription['enabled'] ?? true, $subscriptionPath.'.enabled')) {
                continue;
            }

            $normalizedSubscriptions[] = self::normalizeSubscription(
                $subscription,
                (string) $subscriptionName,
                $subscriptionPath,
                $brokerNames,
                $bestEffort,
            );
        }
        if ($normalizedSubscriptions === []) {
            self::invalid($path.self::MSG_SUBSCRIPTIONS, 'must contain at least one enabled subscription');
        }

        return [
            'name' => $workerName,
            'subscriptions' => $normalizedSubscriptions,
            'scheduler' => [
                'strategy' => 'weighted_fair',
            ],
        ];
    }

    /**
     * @return array{name: string, broker: string, queue: string, weight: int, priority_class: int, prefetch: int, starvation_after: int, early_ack: bool, no_ack: bool}
     */
    private static function normalizeSubscription(
        mixed $subscription,
        string $subscriptionName,
        string $subscriptionPath,
        array $brokerNames,
        bool $bestEffort,
    ): array {
        $broker = self::string(
            $subscription['broker'] ?? null,
            $subscriptionPath.self::MSG_BROKER,
        );
        if (! isset($brokerNames[$broker])) {
            self::invalid($subscriptionPath.self::MSG_BROKER, 'references an unknown broker');
        }

        $prefetch = self::prefetch(
            $subscription['prefetch'] ?? ['mode' => 'fixed', 'value' => 16],
            $subscriptionPath.'.prefetch',
        );

        $earlyAck = self::boolean(
            $subscription['early_ack'] ?? false,
            $subscriptionPath.'.early_ack',
        );
        $noAck = self::validateAckFlags(
            $subscription['no_ack'] ?? false,
            $earlyAck,
            $bestEffort,
            $subscriptionName,
            $subscriptionPath,
        );

        return [
            'name' => $subscriptionName,
            'broker' => $broker,
            'queue' => self::string(
                $subscription['queue'] ?? null,
                $subscriptionPath.'.queue',
            ),
            'weight' => self::positiveInt(
                $subscription['weight'] ?? 1,
                $subscriptionPath.'.weight',
                65535,
            ),
            'priority_class' => self::boundedI16(
                $subscription['priority_class'] ?? 0,
                $subscriptionPath.'.priority_class',
            ),
            'prefetch' => $prefetch,
            'starvation_after' => self::positiveInt(
                $subscription['starvation_after'] ?? 30,
                $subscriptionPath.'.starvation_after',
            ),
            'early_ack' => $earlyAck,
            'no_ack' => $noAck,
        ];
    }

    private static function validateAckFlags(
        mixed $noAckRaw,
        bool $earlyAck,
        bool $bestEffort,
        string $subscriptionName,
        string $subscriptionPath,
    ): bool {
        if ($earlyAck && ! $bestEffort) {
            self::invalid(
                $subscriptionPath.'.early_ack',
                'early_ack is not allowed in reliable mode — set best_effort=true to opt in',
            );
        }

        $noAck = self::boolean($noAckRaw, $subscriptionPath.self::MSG_NO_ACK);
        if (! $noAck) {
            return false;
        }

        if (! $earlyAck) {
            self::invalid(
                $subscriptionPath.self::MSG_NO_ACK,
                "no_ack=true requires early_ack=true for subscription '{$subscriptionName}'",
            );
        }

        if (! $bestEffort) {
            self::invalid(
                $subscriptionPath.self::MSG_NO_ACK,
                "no_ack=true requires best_effort=true for subscription '{$subscriptionName}'",
            );
        }

        return true;
    }

    private static function prefetch(mixed $prefetch, string $path): int
    {
        if (! is_array($prefetch)) {
            self::invalid($path, 'must contain fixed mode and value');
        }
        if (($prefetch['mode'] ?? null) !== 'fixed') {
            self::invalid($path.'.mode', 'must be fixed');
        }

        return self::positiveInt($prefetch['value'] ?? null, $path.'.value', 65535);
    }

    /**
     * @return array{confirms: bool, mandatory: bool, confirm_timeout: int}
     */
    private static function publisher(mixed $publisher): array
    {
        if (! is_array($publisher)) {
            self::invalid('publisher', self::MSG_MUST_BE_ARRAY);
        }

        return [
            'confirms' => self::boolean($publisher['confirms'] ?? true, 'publisher.confirms'),
            'mandatory' => self::boolean($publisher['mandatory'] ?? true, 'publisher.mandatory'),
            'confirm_timeout' => self::positiveInt(
                $publisher['confirm_timeout'] ?? 30000,
                'publisher.confirm_timeout',
            ),
        ];
    }

    /**
     * @return array{mode: string, buckets: list<int>, max_buckets: int, queue_expiry_margin: int, detection_timeout: int}
     */
    private static function delay(mixed $delay): array
    {
        if (! is_array($delay)) {
            self::invalid('delay', self::MSG_MUST_BE_ARRAY);
        }

        $mode = $delay['mode'] ?? 'auto';
        if (! is_string($mode) || ! in_array($mode, ['auto', 'plugin', 'ttl'], true)) {
            self::invalid('delay.mode', 'must be auto, plugin, or ttl');
        }

        $buckets = $delay['buckets'] ?? [1, 5, 30, 120];
        if (! is_array($buckets) || $buckets === []) {
            self::invalid('delay.buckets', 'must contain at least one bucket');
        }
        $normalizedBuckets = [];
        foreach ($buckets as $index => $bucket) {
            $normalizedBuckets[] = self::positiveInt($bucket, "delay.buckets.{$index}");
        }

        $maxBuckets = self::positiveInt($delay['max_buckets'] ?? 8, 'delay.max_buckets');
        if (count($normalizedBuckets) > $maxBuckets) {
            self::invalid('delay.buckets', "bucket count exceeds configured maximum {$maxBuckets}");
        }

        return [
            'mode' => $mode,
            'buckets' => $normalizedBuckets,
            'max_buckets' => $maxBuckets,
            'queue_expiry_margin' => self::positiveInt(
                $delay['queue_expiry_margin'] ?? 60,
                'delay.queue_expiry_margin',
            ),
            'detection_timeout' => self::positiveInt(
                $delay['detection_timeout'] ?? 5,
                'delay.detection_timeout',
            ),
        ];
    }

    /**
     * @return array<string, mixed>
     */
    private static function topology(mixed $topology): array
    {
        if (! is_array($topology)) {
            self::invalid('topology', self::MSG_MUST_BE_ARRAY);
        }

        $queue = $topology['queue'] ?? [];
        if (! is_array($queue)) {
            self::invalid('topology.queue', self::MSG_MUST_BE_ARRAY);
        }

        $type = $queue['type'] ?? 'quorum';
        if (! is_string($type) || ! in_array($type, ['quorum', 'classic'], true)) {
            self::invalid('topology.queue.type', 'must be quorum or classic');
        }

        $deadLetter = $topology['dead_letter'] ?? null;
        $normalizedDeadLetter = null;
        if ($deadLetter !== null) {
            if (! is_array($deadLetter)) {
                self::invalid('topology.dead_letter', 'must be null or an array');
            }

            $deadLetterPath = 'topology.dead_letter';
            $exchange = self::string($deadLetter['exchange'] ?? null, $deadLetterPath.'.exchange');
            $dlqQueue = self::string($deadLetter['queue'] ?? null, $deadLetterPath.'.queue');
            $routingKey = $deadLetter['routing_key'] ?? null;
            if ($routingKey !== null && (! is_string($routingKey) || $routingKey === '')) {
                self::invalid($deadLetterPath.'.routing_key', 'must be null or a non-empty string');
            }

            $normalizedDeadLetter = [
                'enabled' => true,
                'exchange' => $exchange,
                'queue' => $dlqQueue,
                'routing_key' => $routingKey,
            ];
        }

        $deliveryLimit = $queue['delivery_limit'] ?? null;
        if ($deliveryLimit !== null) {
            $deliveryLimit = self::positiveInt($deliveryLimit, 'topology.queue.delivery_limit');
        }

        if ($deliveryLimit !== null && $normalizedDeadLetter === null) {
            self::invalid(
                'topology.dead_letter',
                'dead_letter must be configured when delivery_limit is set — '
                .'without it, poison messages are silently dropped by the quorum queue',
            );
        }

        return [
            'queue' => [
                'type' => $type,
                'durable' => self::boolean($queue['durable'] ?? true, 'topology.queue.durable'),
                'delivery_limit' => $deliveryLimit,
            ],
            'dead_letter' => $normalizedDeadLetter,
        ];
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
        if (! is_bool($value)) {
            self::invalid($path, 'must be a boolean');
        }

        return $value;
    }

    private static function positiveInt(mixed $value, string $path, ?int $max = null): int
    {
        if (! is_int($value) || $value < 1) {
            self::invalid($path, 'must be a positive integer');
        }
        if ($max !== null && $value > $max) {
            self::invalid($path, 'must be at most '.$max);
        }

        return $value;
    }

    private static function boundedI16(mixed $value, string $path): int
    {
        if (! is_int($value) || $value < -32768 || $value > 32767) {
            self::invalid($path, 'must be an integer between -32768 and 32767');
        }

        return $value;
    }

    private static function name(mixed $name, string $path): string
    {
        if (! is_string($name) || $name === '') {
            self::invalid($path, 'keys must be non-empty strings');
        }

        return $name;
    }

    private static function invalid(string $path, string $message): never
    {
        throw new InvalidArgumentException($path.': '.$message);
    }
}
