<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Tests\TestCase;

uses(TestCase::class)->in(__DIR__);

function validConfig(): array
{
    return [
        'brokers' => [
            'orders_eu' => [
                'uri' => 'amqp://rabbit_rs:rabbit_rs_lab@127.0.0.1:5672/orders-eu',
                'vhosts' => ['/'],
            ],
        ],
        'routes' => [
            'default' => ['broker' => 'orders_eu'],
        ],
        'workers' => [
            'default' => [
                'broker' => 'orders_eu',
                'queue' => 'test-queue',
                'prefetch' => 16,
            ],
        ],
        'publishers' => [
            'default' => ['broker' => 'orders_eu'],
        ],
        'topology' => 'declare',
    ];
}
