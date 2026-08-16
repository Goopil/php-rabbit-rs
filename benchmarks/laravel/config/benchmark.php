<?php

declare(strict_types=1);

use Drivers\DatabaseDriver;
use Drivers\PhpAmqplibDriver;
use Drivers\RabbitRsDriver;
use Drivers\RedisDriver;
use Drivers\VyuldashevDriver;

return [
    'drivers' => [
        'rabbit-rs' => [
            'class' => RabbitRsDriver::class,
            'connection' => env('BENCH_RABBIT_RS_DSN', 'amqp://guest:guest@127.0.0.1:5672/'),
            'queue' => env('BENCH_RABBIT_RS_QUEUE', 'bench.rabbit-rs'),
            'exchange' => env('BENCH_RABBIT_RS_EXCHANGE', 'bench.rabbit-rs'),
        ],
        'php-amqplib' => [
            'class' => PhpAmqplibDriver::class,
            'host' => env('BENCH_AMQPLIB_HOST', '127.0.0.1'),
            'port' => (int) env('BENCH_AMQPLIB_PORT', 5672),
            'user' => env('BENCH_AMQPLIB_USER', 'guest'),
            'password' => env('BENCH_AMQPLIB_PASSWORD', 'guest'),
            'vhost' => env('BENCH_AMQPLIB_VHOST', '/'),
            'queue' => env('BENCH_AMQPLIB_QUEUE', 'bench.amqplib'),
            'exchange' => env('BENCH_AMQPLIB_EXCHANGE', 'bench.amqplib'),
        ],
        'vyuldashev' => [
            'class' => VyuldashevDriver::class,
            'connection' => env('BENCH_VYULDASHEV_DSN', 'amqp://guest:guest@127.0.0.1:5672/'),
            'queue' => env('BENCH_VYULDASHEV_QUEUE', 'bench.vyuldashev'),
        ],
        'redis' => [
            'class' => RedisDriver::class,
            'host' => env('BENCH_REDIS_HOST', '127.0.0.1'),
            'port' => (int) env('BENCH_REDIS_PORT', 6379),
            'queue' => env('BENCH_REDIS_QUEUE', 'bench.redis'),
        ],
        'database' => [
            'class' => DatabaseDriver::class,
            'connection' => env('BENCH_DB_CONNECTION', 'sqlite'),
            'database' => env('BENCH_DB_DATABASE', sys_get_temp_dir() . '/bench-laravel.sqlite'),
            'queue' => env('BENCH_DB_QUEUE', 'bench.database'),
        ],
    ],

    'payload_sizes' => env('BENCH_PAYLOAD_SIZES', '256,1024,10240,102400'),
    'batch_sizes' => env('BENCH_BATCH_SIZES', '1,16,64,256'),
    'message_counts' => [
        'smoke' => (int) env('BENCH_SMOKE_COUNT', 50),
        'full' => (int) env('BENCH_FULL_COUNT', 5000),
    ],

    'modes' => ['cli', 'fpm', 'octane'],
    'mode' => env('BENCH_MODE', 'cli'),

    'rabbit-rs-config' => [
        'topology_mode' => 'declare',
        'brokers' => [
            'default' => [
                'hosts' => env('BENCH_RABBIT_RS_HOSTS', '127.0.0.1:5672'),
                'vhost' => env('BENCH_RABBIT_RS_VHOST', '/'),
                'credentials' => [
                    'username' => env('BENCH_RABBIT_RS_USER', 'guest'),
                    'password' => env('BENCH_RABBIT_RS_PASSWORD', 'guest'),
                ],
                'heartbeat' => 30,
            ],
        ],
        'routes' => [
            'default' => [
                'broker' => 'default',
                'exchange' => env('BENCH_RABBIT_RS_EXCHANGE', 'bench.rabbit-rs'),
                'routing_key' => '{queue}',
            ],
        ],
        'workers' => [
            'default' => [
                'scheduler' => [
                    'strategy' => 'weighted_fair',
                    'max_in_flight' => 64,
                ],
                'subscriptions' => [
                    'default' => [
                        'enabled' => true,
                        'broker' => 'default',
                        'queue' => env('BENCH_RABBIT_RS_QUEUE', 'bench.rabbit-rs'),
                        'weight' => 1,
                        'priority_class' => 0,
                        'prefetch' => [
                            'mode' => 'fixed',
                            'value' => 16,
                        ],
                        'starvation_after' => 30,
                    ],
                ],
            ],
        ],
        'publisher' => [
            'confirms' => true,
            'mandatory' => true,
            'confirm_timeout' => 30000,
        ],
        'delay' => [
            'mode' => 'auto',
            'buckets' => [1, 5, 30, 120],
            'max_buckets' => 8,
            'queue_expiry_margin' => 60,
            'detection_timeout' => 5,
        ],
        'topology' => [
            'queue' => [
                'type' => 'quorum',
                'durable' => true,
                'delivery_limit' => 20,
            ],
            'dead_letter' => null,
        ],
    ],
];
