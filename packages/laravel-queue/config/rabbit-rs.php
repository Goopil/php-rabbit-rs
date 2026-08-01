<?php

declare(strict_types=1);

return [
    'topology_mode' => env('RABBIT_RS_TOPOLOGY_MODE', 'declare'),

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
            ],
            'heartbeat' => (int) env('RABBIT_RS_HEARTBEAT', 30),
        ],
    ],

    'routes' => [
        'default' => [
            'broker' => 'default',
            'exchange' => env('RABBIT_RS_EXCHANGE', 'laravel.jobs'),
            'routing_key' => '{queue}',
        ],
    ],

    'workers' => [
        'default' => [
            'scheduler' => [
                'strategy' => 'weighted_fair',
                'max_in_flight' => (int) env('RABBIT_RS_MAX_IN_FLIGHT', 64),
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
                        'value' => (int) env('RABBIT_RS_PREFETCH', 16),
                    ],
                    'starvation_after' => 30,
                ],
            ],
        ],
    ],

    'publisher' => [
        'confirms' => true,
        'mandatory' => true,
    ],

    'topology' => [
        'queue' => [
            'type' => 'quorum',
            'durable' => true,
            'delivery_limit' => 20,
        ],
        'dead_letter' => null,
    ],
];