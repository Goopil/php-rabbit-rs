<?php

declare(strict_types=1);
/*
| Rabbit RS — Cross-Cutting Defaults.
|
| Per-connection configuration lives in config/queue.php under
| queue.connections.* with `driver => 'rabbit-rs'` (one connection = one
| broker = one native pool). This file only holds cross-cutting defaults
| merged under every rabbit-rs connection: a key the connection omits is
| inherited from here (per sub-key for tls, delay and dead_letter), and
| env() strings are cast/validated lazily at connection resolution
| (queue.connections.<name>.<key>).
*/

return [
    'heartbeat' => env('RABBIT_RS_HEARTBEAT', 30),
    'tls' => [
        'enabled' => env('RABBIT_RS_TLS', false),
        'ca_cert' => env('RABBIT_RS_TLS_CA_CERT'),
        'client_cert' => env('RABBIT_RS_TLS_CLIENT_CERT'),
        'client_key' => env('RABBIT_RS_TLS_CLIENT_KEY'),
    ],

    // safe (confirms + mandatory) | unsafe (no confirms) | blind (fire-and-forget)
    'safety' => env('RABBIT_RS_SAFETY', 'safe'),
    'confirm_timeout' => env('RABBIT_RS_CONFIRM_TIMEOUT', 30000),
    // int | ['mode' => 'fixed', 'value' => N] | ['mode' => 'adaptive', 'initial' => N, 'min' => N, 'max' => N, 'target_buffer_seconds' => N]
    // adaptive learns the job duration (EWMA of ack latency) and keeps about
    // target_buffer_seconds of ready work buffered between min and max;
    // requires acknowledgements (early_ack and no_ack must be false).
    'prefetch' => env('RABBIT_RS_PREFETCH', 64),
    'wait_timeout' => env('RABBIT_RS_CONSUMER_WAIT_TIMEOUT', 30000),
    // declare | verify | external
    'topology_mode' => env('RABBIT_RS_TOPOLOGY_MODE', 'declare'),
    // quorum | classic
    'queue_type' => 'quorum',
    'queue_durable' => true,
    'delivery_limit' => null,
    // e.g. ['exchange' => 'dlx.jobs', 'queue' => 'dead.jobs']
    'dead_letter' => null,
    // auto | plugin | ttl
    'delay' => [
        'mode' => env('RABBIT_RS_DELAY_MODE', 'auto'),
        'buckets' => array_map('intval', array_filter(array_map('trim', explode(',', env('RABBIT_RS_DELAY_BUCKETS', '1,5,30,120'))))),
        'max_buckets' => env('RABBIT_RS_DELAY_MAX_BUCKETS', 8),
        'queue_expiry_margin' => env('RABBIT_RS_DELAY_QUEUE_EXPIRY_MARGIN', 60),
    ],
    // default | horizon
    'worker' => env('RABBIT_RS_WORKER', 'default'),
    // auto_subscribe was removed (v1): runtime worker-profile registration is
    // not supported. The connection compiler rejects the key with guidance —
    // declare queues explicitly via the connection `queue` key or the
    // `subscriptions` escape hatch. Multi-queue pop scoping is unaffected.
    'production_warning' => env('RABBIT_RS_PRODUCTION_WARNING', true),
    'best_effort' => env('RABBIT_RS_BEST_EFFORT', false),
    // Worker health statefiles read by `rabbit-rs:probe` (one JSON file per worker PID)
    'probes' => [
        'path' => env('RABBIT_RS_PROBES_PATH', storage_path('framework/rabbit-rs/probes')),
    ],
];
