<?php

return [
    'default' => env('QUEUE_CONNECTION', 'rabbit-rs'),
    'connections' => [
        'sync' => [
            'driver' => 'sync',
        ],
        'rabbit-rs' => [
            'driver' => 'rabbit-rs',
            'queue' => env('RABBIT_RS_QUEUE', 'default'),
            // Must match the lab's permission regex for user rabbit_rs
            // (^rabbit-rs\.): a mismatched name is refused at declare time
            // and the connection transitions to failed-permanent.
            'exchange' => env('RABBIT_RS_EXCHANGE', 'rabbit-rs.jobs'),
            'hosts' => env('RABBIT_RS_HOSTS', '127.0.0.1:5672'),
            'vhost' => env('RABBIT_RS_VHOST', '/'),
            'username' => env('RABBIT_RS_USERNAME', 'guest'),
            'password' => env('RABBIT_RS_PASSWORD', 'guest'),
            // Large on purpose: publications must stay parked in the publish
            // buffer across the whole publish→reload/stop window so the
            // harness proves the reload and graceful-stop flush paths
            // (the age timer must never be the path that delivers them).
            'flush_interval' => (int) env('RABBIT_RS_FLUSH_INTERVAL', 60000),
            'after_commit' => false,
        ],
    ],
    'failed' => [
        'driver' => env('QUEUE_FAILED_DRIVER', 'null'),
    ],
    'batching' => [
        'database' => env('DB_CONNECTION', 'sqlite'),
        'table' => 'job_batches',
    ],
];
