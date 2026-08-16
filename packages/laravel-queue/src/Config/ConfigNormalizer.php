<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Config;

use InvalidArgumentException;

final class ConfigNormalizer
{
    private const DEFAULT_AMQP_PORT = 5672;

    /**
     * @param array<string, mixed> $config
     * @return array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array<string, bool>,
     *     topology: array<string, mixed>
     * }
     */
    public static function normalize(array $config): array
    {
        $topologyMode = self::topologyMode($config['topology_mode'] ?? 'declare');
        $brokers = self::brokers($config['brokers'] ?? null);
        $brokerNames = array_fill_keys(array_column($brokers, 'name'), true);
        $topology = self::topology($config['topology'] ?? []);

        return [
            'native' => [
                'brokers' => $brokers,
                'workers' => self::workers($config['workers'] ?? [], $brokerNames),
                'topology_mode' => $topologyMode,
                'delay' => self::delay($config['delay'] ?? []),
                'dead_letter' => $topology['dead_letter'],
                'delivery_limit' => $topology['queue']['delivery_limit'],
            ],
            'routes' => self::routes($config['routes'] ?? [], $brokerNames),
            'publisher' => self::publisher($config['publisher'] ?? []),
            'topology' => $topology,
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
                self::invalid($path, 'must be an array');
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
                self::invalid($path.'.credentials', 'must be an array');
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
            if (preg_match('/^\[([^]]+)](?::([0-9]+))?$/', $endpoint, $matches) !== 1) {
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
            self::invalid($path, 'must be an array');
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
            self::invalid($path.'.ca_cert', 'must be null or a string');
        }

        $clientCert = $tls['client_cert'] ?? null;
        if ($clientCert !== null && ! is_string($clientCert)) {
            self::invalid($path.'.client_cert', 'must be null or a string');
        }

        $clientKey = $tls['client_key'] ?? null;
        if ($clientKey !== null && ! is_string($clientKey)) {
            self::invalid($path.'.client_key', 'must be null or a string');
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
            self::invalid('routes', 'must be an array');
        }

        ksort($routes);
        $normalized = [];
        foreach ($routes as $name => $route) {
            $path = 'routes.'.self::name($name, 'routes');
            if (! is_array($route)) {
                self::invalid($path, 'must be an array');
            }

            $broker = self::string($route['broker'] ?? null, $path.'.broker');
            if (! isset($brokerNames[$broker])) {
                self::invalid($path.'.broker', 'references an unknown broker');
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
    private static function workers(mixed $workers, array $brokerNames): array
    {
        if (! is_array($workers)) {
            self::invalid('workers', 'must be an array');
        }

        ksort($workers);
        $normalized = [];
        foreach ($workers as $name => $worker) {
            $path = 'workers.'.self::name($name, 'workers');
            if (! is_array($worker)) {
                self::invalid($path, 'must be an array');
            }

            $scheduler = $worker['scheduler'] ?? null;
            if (! is_array($scheduler)) {
                self::invalid($path.'.scheduler', 'must be an array');
            }
            if (($scheduler['strategy'] ?? 'weighted_fair') !== 'weighted_fair') {
                self::invalid($path.'.scheduler.strategy', 'must be weighted_fair');
            }
            $maxInFlight = self::boundedU16(
                $scheduler['max_in_flight'] ?? 64,
                $path.'.scheduler.max_in_flight',
            );

            $subscriptions = $worker['subscriptions'] ?? null;
            if (! is_array($subscriptions) || $subscriptions === []) {
                self::invalid($path.'.subscriptions', 'must contain at least one subscription');
            }
            ksort($subscriptions);

            $normalizedSubscriptions = [];
            foreach ($subscriptions as $subscriptionName => $subscription) {
                $subscriptionPath = $path.'.subscriptions.'.self::name(
                    $subscriptionName,
                    $path.'.subscriptions',
                );
                if (! is_array($subscription)) {
                    self::invalid($subscriptionPath, 'must be an array');
                }
                if (! self::boolean($subscription['enabled'] ?? true, $subscriptionPath.'.enabled')) {
                    continue;
                }

                $broker = self::string(
                    $subscription['broker'] ?? null,
                    $subscriptionPath.'.broker',
                );
                if (! isset($brokerNames[$broker])) {
                    self::invalid($subscriptionPath.'.broker', 'references an unknown broker');
                }

                $prefetch = self::prefetch(
                    $subscription['prefetch'] ?? ['mode' => 'fixed', 'value' => 16],
                    $subscriptionPath.'.prefetch',
                );
                if ($maxInFlight < $prefetch) {
                    self::invalid(
                        $path.'.scheduler.max_in_flight',
                        'must be at least every subscription prefetch',
                    );
                }

                $normalizedSubscriptions[] = [
                    'name' => (string) $subscriptionName,
                    'broker' => $broker,
                    'queue' => self::string(
                        $subscription['queue'] ?? null,
                        $subscriptionPath.'.queue',
                    ),
                    'weight' => self::boundedU16(
                        $subscription['weight'] ?? 1,
                        $subscriptionPath.'.weight',
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
                ];
            }
            if ($normalizedSubscriptions === []) {
                self::invalid($path.'.subscriptions', 'must contain at least one enabled subscription');
            }

            $normalized[] = [
                'name' => (string) $name,
                'subscriptions' => $normalizedSubscriptions,
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => $maxInFlight,
                ],
            ];
        }

        return $normalized;
    }

    private static function prefetch(mixed $prefetch, string $path): int
    {
        if (! is_array($prefetch)) {
            self::invalid($path, 'must contain fixed mode and value');
        }
        if (($prefetch['mode'] ?? null) !== 'fixed') {
            self::invalid($path.'.mode', 'must be fixed');
        }

        return self::boundedU16($prefetch['value'] ?? null, $path.'.value');
    }

    /**
     * @return array{confirms: bool, mandatory: bool}
     */
    private static function publisher(mixed $publisher): array
    {
        if (! is_array($publisher)) {
            self::invalid('publisher', 'must be an array');
        }

        return [
            'confirms' => self::boolean($publisher['confirms'] ?? true, 'publisher.confirms'),
            'mandatory' => self::boolean($publisher['mandatory'] ?? true, 'publisher.mandatory'),
        ];
    }

    /**
     * @return array{mode: string, buckets: list<int>, max_buckets: int, queue_expiry_margin: int, detection_timeout: int}
     */
    private static function delay(mixed $delay): array
    {
        if (! is_array($delay)) {
            self::invalid('delay', 'must be an array');
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
            self::invalid('topology', 'must be an array');
        }

        $queue = $topology['queue'] ?? [];
        if (! is_array($queue)) {
            self::invalid('topology.queue', 'must be an array');
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

        return [
            'queue' => [
                'type' => $type,
                'durable' => self::boolean($queue['durable'] ?? true, 'topology.queue.durable'),
                'delivery_limit' => self::positiveInt(
                    $queue['delivery_limit'] ?? 20,
                    'topology.queue.delivery_limit',
                ),
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

    private static function positiveInt(mixed $value, string $path): int
    {
        if (! is_int($value) || $value < 1) {
            self::invalid($path, 'must be a positive integer');
        }

        return $value;
    }

    private static function boundedU16(mixed $value, string $path): int
    {
        $value = self::positiveInt($value, $path);
        if ($value > 65535) {
            self::invalid($path, 'must be at most 65535');
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