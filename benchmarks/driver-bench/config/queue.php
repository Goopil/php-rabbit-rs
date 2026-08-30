<?php

/*
|--------------------------------------------------------------------------
| Driver-bench queue configuration — Phase E (driver-level benchmark)
|--------------------------------------------------------------------------
|
| THREE coexisting RabbitMQ connections, one per driver under test. They
| are never interchangeable within a run: bench.php always targets exactly
| one via --connection=.
|
| Fairness allocation (lab grants, definitions.json d35580c — unmodified):
|   rabbit-rs        → goopil/rabbit-rs-laravel (ext-rabbit_rs), vhost "/",      user rabbit_rs
|   rabbitmq-amqplib → vladimir-yuldashev (php-amqplib),          vhost /orders-eu, user admin
|   rabbitmq-ext     → iamfarhad/laravel-rabbitmq (ext-amqp),     vhost /billing,   user admin
|
| Both third-party drivers register the generic 'rabbitmq' driver name;
| bootstrap/app.php re-registers each under the unambiguous names used
| below so the three drivers can never shadow each other.
|
| The rabbit-rs connection is configured through the published
| config/rabbit-rs.php file (driver global config) plus the minimal
| connection entry below.
|
*/

return [

    'default' => env('QUEUE_CONNECTION', 'rabbit-rs'),

    'connections' => [

        // ------------------------------------------------------------------
        // goopil/rabbit-rs-laravel — native ext-rabbit_rs (Rust) driver
        // Broker/topology/publisher settings live in config/rabbit-rs.php:
        // vhost "/", user rabbit_rs, default exchange (''), classic durable
        // queue declared by the driver (topology_mode=declare).
        // ------------------------------------------------------------------
        'rabbit-rs' => [
            'driver' => 'rabbit-rs',
            'queue' => env('RABBIT_RS_QUEUE', 'bench.goopil.driver-bench'),
            'after_commit' => false,
            // Pop blocking window in seconds (worker-style): lets the pull
            // consumer wait for prefetch-window refills instead of
            // busy-null-spinning, mirroring `queue:work --sleep/--timeout`.
            'block_for' => env('RABBIT_RS_BLOCK_FOR') !== null ? (int) env('RABBIT_RS_BLOCK_FOR') : null,
        ],

        // ------------------------------------------------------------------
        // vladimir-yuldashev/laravel-queue-rabbitmq — pure-PHP php-amqplib
        // Full connection config spelled out here (the package only merges
        // defaults into a connection literally named 'rabbitmq'). Driver
        // defaults otherwise: empty options array, default exchange (''),
        // durable classic queue auto-declared on first push, pop = basic_get.
        // ------------------------------------------------------------------
        'rabbitmq-amqplib' => [
            'driver' => 'rabbitmq-amqplib',
            'queue' => env('VLADIMIR_QUEUE', 'bench.vladimir.driver-bench'),
            'connection' => 'default',
            'hosts' => [
                [
                    'host' => env('VLADIMIR_HOST', '127.0.0.1'),
                    'port' => (int) env('VLADIMIR_PORT', 5672),
                    'user' => env('VLADIMIR_LOGIN', 'guest'),
                    'password' => env('VLADIMIR_PASSWORD', 'guest'),
                    'vhost' => env('VLADIMIR_VHOST', '/'),
                ],
            ],
            'options' => [],
            'worker' => 'default',
        ],

        // ------------------------------------------------------------------
        // iamfarhad/laravel-rabbitmq — C ext-amqp driver (Docker only).
        // Mirrors the package's shipped config/rabbitmq.php defaults with
        // env remapped to IAMFARHAD_* so the two php-amqplib-based drivers
        // can coexist in a single .env. Default exchange (''), durable
        // classic queue auto-declared on push/pop, pop = basic_get (poll).
        // Publisher confirms exist but default to OFF — enable the confirms
        // variant with IAMFARHAD_PUBLISHER_CONFIRMS=true.
        // ------------------------------------------------------------------
        'rabbitmq-ext' => [
            'driver' => 'rabbitmq-ext',
            'queue' => env('IAMFARHAD_QUEUE', 'bench.iamfarhad.driver-bench'),
            'after_commit' => false,
            'worker' => 'default',
            'connection_name' => env('IAMFARHAD_CONNECTION_NAME', 'driver-bench-rabbitmq-ext'),
            'hosts' => [
                'host' => env('IAMFARHAD_HOST', '127.0.0.1'),
                'port' => (int) env('IAMFARHAD_PORT', 5672),
                'user' => env('IAMFARHAD_USERNAME', 'guest'),
                'password' => env('IAMFARHAD_PASSWORD', 'guest'),
                'vhost' => env('IAMFARHAD_VHOST', '/'),
                'lazy' => (bool) env('IAMFARHAD_LAZY_CONNECTION', true),
                'heartbeat' => (int) env('IAMFARHAD_HEARTBEAT', 60),
                'connect_timeout' => (int) env('IAMFARHAD_CONNECT_TIMEOUT', 10),
                'read_timeout' => (int) env('IAMFARHAD_READ_TIMEOUT', 0),
                'write_timeout' => (int) env('IAMFARHAD_WRITE_TIMEOUT', 0),
                'secure' => (bool) env('IAMFARHAD_SECURE', false),
            ],
            'pool' => [
                'max_connections' => (int) env('IAMFARHAD_MAX_CONNECTIONS', 10),
                'min_connections' => (int) env('IAMFARHAD_MIN_CONNECTIONS', 2),
                'max_channels_per_connection' => (int) env('IAMFARHAD_MAX_CHANNELS_PER_CONNECTION', 100),
                'max_retries' => (int) env('IAMFARHAD_MAX_RETRIES', 3),
                'retry_delay' => (int) env('IAMFARHAD_RETRY_DELAY', 1000),
                'lazy' => (bool) env('IAMFARHAD_LAZY_POOL', true),
                'health_check_enabled' => (bool) env('IAMFARHAD_HEALTH_CHECK_ENABLED', true),
                'health_check_interval' => (int) env('IAMFARHAD_HEALTH_CHECK_INTERVAL', 30),
            ],
            'exchange' => env('IAMFARHAD_EXCHANGE', ''),
            'exchange_type' => env('IAMFARHAD_EXCHANGE_TYPE', 'direct'),
            'exchange_routing_key' => env('IAMFARHAD_EXCHANGE_ROUTING_KEY', '%s'),
            'prioritize_delayed' => (bool) env('IAMFARHAD_PRIORITIZE_DELAYED', false),
            'queue_max_priority' => (int) env('IAMFARHAD_QUEUE_MAX_PRIORITY', 10),
            'quorum' => (bool) env('IAMFARHAD_QUEUE_QUORUM', false),
            'reroute_failed' => (bool) env('IAMFARHAD_REROUTE_FAILED', false),
            'failed_exchange' => env('IAMFARHAD_FAILED_EXCHANGE', ''),
            'failed_routing_key' => env('IAMFARHAD_FAILED_ROUTING_KEY', '%s.failed'),
            'delay_queue_granularity' => (int) env('IAMFARHAD_DELAY_QUEUE_GRANULARITY', 1000),
            'failed' => [
                'ownership' => env('IAMFARHAD_FAILED_OWNERSHIP', 'broker'),
                'exchange' => env('IAMFARHAD_FAILED_MESSAGES_EXCHANGE', 'failed_messages'),
            ],
            'backoff' => [
                'base_delay' => (int) env('IAMFARHAD_BACKOFF_BASE_DELAY', 1000),
                'max_delay' => (int) env('IAMFARHAD_BACKOFF_MAX_DELAY', 60000),
                'multiplier' => (float) env('IAMFARHAD_BACKOFF_MULTIPLIER', 2.0),
                'jitter' => (bool) env('IAMFARHAD_BACKOFF_JITTER', true),
            ],
            'queues' => [
                'default' => [
                    'durable' => (bool) env('IAMFARHAD_QUEUE_DURABLE', true),
                    'auto_delete' => (bool) env('IAMFARHAD_QUEUE_AUTO_DELETE', false),
                    'priority' => null,
                    'arguments' => [],
                    'bindings' => [],
                ],
            ],
            'dead_letter' => [
                'enabled' => (bool) env('IAMFARHAD_DLX_ENABLED', true),
                'exchange' => env('IAMFARHAD_DLX_EXCHANGE', 'dlx'),
                'exchange_type' => env('IAMFARHAD_DLX_EXCHANGE_TYPE', 'direct'),
                'queue_suffix' => env('IAMFARHAD_DLX_QUEUE_SUFFIX', '.dlq'),
                'ttl' => null,
            ],
            'delayed_message' => [
                'exchange' => env('IAMFARHAD_DELAYED_EXCHANGE', 'delayed'),
                'exchange_type' => env('IAMFARHAD_DELAYED_EXCHANGE_TYPE', 'direct'),
                'plugin_enabled' => (bool) env('IAMFARHAD_DELAYED_PLUGIN_ENABLED', false),
            ],
            'rpc' => [
                'enabled' => (bool) env('IAMFARHAD_RPC_ENABLED', false),
                'timeout' => (int) env('IAMFARHAD_RPC_TIMEOUT', 30),
                'callback_queue_prefix' => env('IAMFARHAD_RPC_CALLBACK_PREFIX', ''),
            ],
            'publisher_confirms' => [
                'enabled' => (bool) env('IAMFARHAD_PUBLISHER_CONFIRMS', false),
                'timeout' => (int) env('IAMFARHAD_PUBLISHER_CONFIRMS_TIMEOUT', 5),
                'mandatory' => (bool) env('IAMFARHAD_PUBLISHER_CONFIRMS_MANDATORY', false),
            ],
            'transactions' => [
                'enabled' => (bool) env('IAMFARHAD_TRANSACTIONS_ENABLED', false),
            ],
            'octane' => [
                'reset_on_request' => (bool) env('IAMFARHAD_OCTANE_RESET_ON_REQUEST', false),
            ],
            'options' => [
                'read_timeout' => (int) env('IAMFARHAD_READ_TIMEOUT', 0),
                'write_timeout' => (int) env('IAMFARHAD_WRITE_TIMEOUT', 0),
                'connect_timeout' => (int) env('IAMFARHAD_CONNECT_TIMEOUT', 10),
                'ssl_options' => [
                    'cafile' => env('IAMFARHAD_SSL_CAFILE', null),
                    'local_cert' => env('IAMFARHAD_SSL_LOCALCERT', null),
                    'local_key' => env('IAMFARHAD_SSL_LOCALKEY', null),
                    'verify_peer' => (bool) env('IAMFARHAD_SSL_VERIFY_PEER', true),
                ],
                'queue' => [
                    'job' => iamfarhad\LaravelRabbitMQ\Jobs\RabbitMQJob::class,
                    'consume_mode' => env('IAMFARHAD_CONSUME_MODE', 'poll'),
                    'lazy' => (bool) env('IAMFARHAD_QUEUE_LAZY', false),
                    'qos' => [
                        'prefetch_size' => (int) env('IAMFARHAD_PREFETCH_SIZE', 0),
                        // basic.qos only governs basic.consume deliveries;
                        // bench pop() uses basic_get (poll mode).
                        'prefetch_count' => (int) env('IAMFARHAD_PREFETCH', 64),
                    ],
                ],
            ],
        ],

    ],

];
