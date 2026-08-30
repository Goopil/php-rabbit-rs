<?php

declare(strict_types=1);

use Illuminate\Support\Env;

/*
|--------------------------------------------------------------------------
| Rabbit RS — Native RabbitMQ Queue Driver
|--------------------------------------------------------------------------
|
| This file is the entry point for configuring the Rabbit RS native queue
| driver. The driver delegates all AMQP I/O to a Rust extension that
| manages connection pools, publisher confirms, consumer scheduling,
| and automatic recovery — none of which runs in PHP userspace.
|
| The config is normalised at runtime by ConfigNormalizer, which validates
| every value and provides actionable error messages when something is
| misconfigured.
|
*/

return [

    /*
    |--------------------------------------------------------------------------
    | Topology Mode
    |--------------------------------------------------------------------------
    |
    | Controls how the driver interacts with broker topology on startup.
    |
    | - declare: Create exchanges, queues, and bindings if they don't exist.
    |           Use this in development or when the application owns its
    |           topology. DDL is idempotent and matches the config below.
    |
    | - verify:  Check that the declared topology exists but never create.
    |           The driver fails fast if a queue or exchange is missing.
    |           Use this in production where an ops team manages topology.
    |
    | - external: Don't touch topology at all. The broker is expected to be
    |           fully configured externally. The driver only publishes and
    |           consumes.
    |
    */

    'topology_mode' => env('RABBIT_RS_TOPOLOGY_MODE', 'declare'),

    /*
    |--------------------------------------------------------------------------
    | Brokers
    |--------------------------------------------------------------------------
    |
    | Each broker entry describes a connection pool. A vhost owns a distinct
    | AMQP connection — publisher channels are pooled, consumer channels are
    | dedicated. You can define multiple brokers to connect to different
    | vhosts or clusters simultaneously.
    |
    | hosts:       Comma-separated list of host:port endpoints. The pool
    |              connects to the first available and fails over on recovery.
    |              IPv6 addresses must be bracketed: [::1]:5672
    |
    | vhost:       AMQP virtual host. Each unique vhost gets its own
    |              connection within the pool.
    |
    | credentials: Plain username/password. Never logged or exposed in Debug.
    |
    | tls:         TLS settings. Set enabled=true for amqps://. The ca_cert
    |              is required when verify=peer. client_cert and client_key
    |              enable mTLS. server_name sets the SNI expectation.
    |
    | heartbeat:   AMQP heartbeat in seconds. If no data is exchanged for
    |              2× this interval, the connection is considered dead and
    |              recovery kicks in. Keep below your TCP keepalive threshold.
    |
    */

    'brokers' => [
        'default' => [
            'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
            'vhost' => env('RABBIT_RS_VHOST', '/'),
            'credentials' => [
                'username' => env('RABBIT_RS_USERNAME', 'guest'),
                'password' => env('RABBIT_RS_PASSWORD', 'guest'),
            ],
            'tls' => [
                'enabled' => (bool) env('RABBIT_RS_TLS', false),
                'server_name' => env('RABBIT_RS_TLS_SERVER_NAME'),
                'ca_cert' => env('RABBIT_RS_TLS_CA_CERT'),
                'client_cert' => env('RABBIT_RS_TLS_CLIENT_CERT'),
                'client_key' => env('RABBIT_RS_TLS_CLIENT_KEY'),
                'verify' => env('RABBIT_RS_TLS_VERIFY', 'peer'),
            ],
            'heartbeat' => (int) env('RABBIT_RS_HEARTBEAT', 30),
        ],
    ],

    /*
    |--------------------------------------------------------------------------
    | Routes
    |--------------------------------------------------------------------------
    |
    | Routes map Laravel queue names to AMQP exchanges and routing keys.
    | When you dispatch a job to a queue, the driver looks up the route by
    | queue name to determine which exchange to publish to.
    |
    | broker:      Must reference a broker defined above.
    |
    | exchange:    The AMQP exchange to publish to. Set to null for the
    |              default exchange (direct routing to queue name).
    |
    | routing_key: The routing key pattern. The placeholder {queue} is
    |              replaced with the actual queue name at publish time.
    |              For example, "{queue}" routes each job to its own queue
    |              name. Use a fixed string for topic-based routing.
    |
    */

    'routes' => [
        'default' => [
            'broker' => 'default',
            'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
            'routing_key' => '{queue}',
        ],
    ],

    /*
    |--------------------------------------------------------------------------
    | Best-Effort Mode
    |--------------------------------------------------------------------------
    |
    | When true, the driver allows subscriptions to use early_ack, which
    | acknowledges deliveries to the broker before PHP processes them. This
    | improves throughput at the cost of at-least-once: if the PHP process
    | crashes, in-flight messages are lost.
    |
    | When false (default), early_ack is rejected at config validation time.
    | This preserves the at-least-once delivery contract.
    |
    */

    'best_effort' => (bool) env('RABBIT_RS_BEST_EFFORT', false),

    /*
    |--------------------------------------------------------------------------
    | Workers
    |--------------------------------------------------------------------------
    |
    | Each worker profile defines a set of subscriptions consumed by a
    | single `rabbit-rs:work` process. The native scheduler multiplexes
    | subscriptions on a dedicated channel per consumer.
    |
    | scheduler.strategy:     Currently only "weighted_fair" is supported.
    |                         The scheduler distributes consumer credit
    |                         across subscriptions proportional to weight.
    |
    | subscriptions:          Each subscription binds the worker to a queue on
    |                         a broker. A worker can subscribe to multiple
    |                         queues with different weights and priorities.
    |
    |   enabled:            Set to false to skip this subscription without
    |                       removing it from config.
    |
    |   broker:              Must reference a broker defined above.
    |
    |   queue:               The AMQP queue to consume from.
    |
    |   weight:              Relative weight in the weighted-fair scheduler.
    |                       Higher weight gets more consumer credit.
    |
    |   priority_class:      Integer priority class (-32768 to 32767).
    |                       Lower numbers are higher priority on quorum
    |                       queues. The scheduler groups subscriptions by
    |                       class and serves the highest priority first.
    |
    |   prefetch.mode:       Currently only "fixed" is supported.
    |   prefetch.value:      QoS prefetch count. The broker delivers at most
    |                       this many unacked messages per consumer channel.
    |
    |   starvation_after:    Seconds without a delivery before the scheduler
    |                       boosts this subscription's weight to prevent
    |                       starvation by heavier-weight subscriptions.
    |
    |   early_ack:           When true, deliveries are auto-acked before
    |                       dispatch to PHP. Requires best_effort=true.
    |                       If PHP crashes, in-flight messages are lost.
    |
    |   no_ack:              When true + early_ack=true + best_effort=true,
    |                       the broker auto-acks deliveries internally — no
    |                       ack frames are sent at all. Eliminates all ack
    |                       round-trips for maximum throughput.
    |
    */

    'workers' => [
        'default' => [
            'scheduler' => [
                'strategy' => 'weighted_fair',
            ],
            'subscriptions' => [
                'default' => [
                    'enabled' => true,
                    'broker' => 'default',
                    'queue' => env('RABBIT_RS_QUEUE', 'default'),
                    'weight' => 1,
                    'priority_class' => 0,
                    'prefetch' => [
                        'mode' => 'fixed',
                        'value' => (int) env('RABBIT_RS_PREFETCH', 64),
                    ],
                    'starvation_after' => 30,
                    'early_ack' => false,
                    'no_ack' => false,
                ],
            ],
        ],
    ],

    /*
    |--------------------------------------------------------------------------
    | Publisher
    |--------------------------------------------------------------------------
    |
    | Controls how publishes are confirmed by the broker.
    |
    | safety:          Delivery guarantee level:
    |
    |                  - "safe" (default): confirm mode + mandatory routing.
    |                    At-least-once delivery. Unconfirmed publishes survive
    |                    connection recovery in bounded process memory and are
    |                    replayed with the same message_id and original deadline.
    |
    |                  - "unsafe": synchronous socket write without confirms.
    |                    The message reached the kernel socket buffer, but a
    |                    broker-side failure can still lose it.
    |
    |                  - "blind": explicit fire-and-forget. Publishing hands the
    |                    message to a bounded background pump and returns
    |                    without waiting for any transport outcome — a transport
    |                    failure after the hand-off is a silent loss. Delayed
    |                    jobs (delay_ms > 0) are NOT honored: the pump bypasses
    |                    delay routing and publishes immediately. When set to
    |                    "unsafe" or "blind", this takes precedence over the
    |                    legacy confirms/mandatory flags below.
    |
    | confirms:        When true, every publish is tracked until the broker
    |                  ACKs or returns it. Unconfirmed publishes survive
    |                  connection recovery in bounded process memory and are
    |                  replayed with the same message_id and original deadline.
    |
    | mandatory:       When true, the broker returns messages that cannot be
    |                  routed (no queue matches the routing key). A return
    |                  takes precedence over a following ACK.
    |
    | confirm_timeout: Milliseconds to wait for a confirm before treating the
    |                  publish as failed. Must be a positive integer. A timeout
    |                  resolves the waiter once — it does not mean the message
    |                  was lost, only that confirmation didn't arrive in time.
    |
    */

    'publisher' => [
        'safety' => env('RABBIT_RS_SAFETY', 'safe'),
        'confirms' => true,
        'mandatory' => true,
        'confirm_timeout' => (int) env('RABBIT_RS_CONFIRM_TIMEOUT', 30000),
    ],

    /*
    |--------------------------------------------------------------------------
    | Consumers
    |--------------------------------------------------------------------------
    |
    | Controls how long the driver waits for a consumer handle to become
    | ready (connection + topology + consume registration) before failing.
    |
    | wait_timeout: Milliseconds to wait when acquiring a consumer handle.
    |               Must be between 1 000 (1 second) and 86 400 000 (24
    |               hours). Prevents worker processes from freezing forever
    |               against an unreachable broker — on expiry the acquisition
    |               fails with a connection error that can be retried.
    |
    */

    'consumers' => [
        'wait_timeout' => (int) env('RABBIT_RS_CONSUMER_WAIT_TIMEOUT', 30000),
    ],

    /*
    |--------------------------------------------------------------------------
    | Delayed Messages
    |--------------------------------------------------------------------------
    |
    | Controls how delayed jobs (Job::dispatch()->delay(...)) are handled.
    |
    | mode:   "auto" — Detect whether the rabbitmq_delayed_message_exchange
    |                plugin is installed. If yes, use the plugin (native
    |                delayed exchange). If no, fall back to TTL bucketed
    |                queues.
    |
    |        "plugin" — Always use the delayed exchange plugin. Fails if the
    |                plugin is not installed.
    |
    |        "ttl" — Always use bucketed TTL queues. Creates one queue per
    |                bucket with a per-message TTL and dead-letter to the
    |                target queue.
    |
    | buckets:            Delay thresholds in seconds for TTL bucketing.
    |                     Messages are placed in the bucket whose TTL is the
    |                     smallest value ≥ the requested delay.
    |
    | max_buckets:        Maximum number of bucket queues to create. If the
    |                     bucket list exceeds this, config validation fails.
    |
    | queue_expiry_margin: Seconds of extra TTL added to bucket queues so
    |                     they survive brief broker restarts without expiring
    |                     mid-delay.
    |
    | detection_timeout:  Seconds to wait for the plugin detection probe
    |                     before falling back to TTL mode in "auto".
    |
    */

    'delay' => [
        'mode' => env('RABBIT_RS_DELAY_MODE', 'auto'),
        'buckets' => array_map('intval', array_filter(array_map('trim', explode(',', env('RABBIT_RS_DELAY_BUCKETS', '1,5,30,120'))))),
        'max_buckets' => (int) env('RABBIT_RS_DELAY_MAX_BUCKETS', 8),
        'queue_expiry_margin' => (int) env('RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN', 60),
        'detection_timeout' => (int) env('RABBIT_RS_DELAY_DETECTION_TIMEOUT', 5),
    ],

    /*
    |--------------------------------------------------------------------------
    | Topology
    |--------------------------------------------------------------------------
    |
    | Defines the queue and dead-letter topology the driver declares when
    | topology_mode is "declare".
    |
    | queue.type:           "quorum" (default) — replicated, Raft-based queues
    |                       with delivery limits. Recommended for production.
    |                       "classic" — single-node durable queues.
    |
    | queue.durable:        Whether the queue survives broker restart.
    |
    | queue.delivery_limit: Max redelivery count on quorum queues. After this
    |                       many delivery attempts, the message is dead-lettered
    |                       (if dead_letter is configured) or dropped.
    |
    |                       dead_letter MUST be configured when delivery_limit is
    |                       set. Without a DLX, poison messages are silently
    |                       dropped after the limit is reached — violating the
    |                       at-least-once delivery contract. Set to null to
    |                       disable the delivery limit entirely.
    |
    | dead_letter:          Optional dead-letter exchange + queue config. When
    |                       set, messages that exceed delivery_limit are routed
    |                       here instead of being silently dropped.
    |
    |   exchange:    The dead-letter exchange to publish to.
    |   queue:       The dead-letter queue to bind.
    |   routing_key: Optional routing key override. If null, the original
    |                message's routing key is used.
    |
    */

    'topology' => [
        'queue' => [
            'type' => 'quorum',
            'durable' => true,
            'delivery_limit' => null,
        ],
        'dead_letter' => null,
    ],

    /*
    |--------------------------------------------------------------------------
    | Production Warning
    |--------------------------------------------------------------------------
    |
    | When true (default) and the application runs in the production
    | environment, the driver logs a warning the first time a rabbit-rs
    | queue connection resolves while topology.queue.delivery_limit and
    | topology.dead_letter are both unset: a message that crashes the
    | worker before settlement is then redelivered forever.
    |
    | Set to false to silence the warning, e.g. when unbounded redelivery is
    | an accepted trade-off. It can also be silenced per queue connection
    | with `production_warning => false` in config/queue.php.
    |
    */

    'production_warning' => (bool) env('RABBIT_RS_PRODUCTION_WARNING', true),

    /*
    |--------------------------------------------------------------------------
    | Worker Mode
    |--------------------------------------------------------------------------
    |
    | Controls which queue worker class is used when the connector resolves
    | a connection.
    |
    | - default: Standard RabbitMqQueue. Use this when Horizon is not
    |           installed or when you don't need Horizon integration.
    |
    | - horizon: HorizonRabbitMqQueue. Dispatches Horizon events
    |           (JobPending, JobPushed, JobReserved, JobDeleted) so Rabbit RS
    |           jobs appear in the Horizon dashboard. Requires laravel/horizon.
    |
    */

    'worker' => env('RABBIT_RS_WORKER', 'default'),
];
