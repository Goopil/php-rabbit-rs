<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Integration;

use Goopil\RabbitRs\Laravel\Tests\TestCase as PackageTestCase;

abstract class IntegrationTestCase extends PackageTestCase
{
    protected function setUp(): void
    {
        parent::setUp();

        if (! extension_loaded('rabbit_rs')) {
            $this->markTestSkipped('ext-rabbit_rs is required for integration tests');
        }
    }

    protected function liveConfig(string $queueName): array
    {
        return [
            'topology_mode' => 'declare',
            'brokers' => [
                'default' => [
                    'hosts' => ['127.0.0.1:5672'],
                    'vhost' => '/orders-eu',
                    'credentials' => [
                        'username' => 'rabbit_rs',
                        'password' => 'rabbit_rs_lab',
                    ],
                    'tls' => ['enabled' => false, 'server_name' => null],
                    'heartbeat' => 30,
                ],
            ],
            'routes' => [
                'default' => [
                    'broker' => 'default',
                    'exchange' => '',
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
                            'queue' => $queueName,
                            'weight' => 1,
                            'priority_class' => 0,
                            'prefetch' => ['mode' => 'fixed', 'value' => 16],
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
    }

    protected function uniqueQueue(string $prefix = 'rabbit-rs-it'): string
    {
        return $prefix.'-'.uniqid('', true);
    }

    /**
     * Declare a quorum queue via the RabbitMQ management API before tests.
     */
    protected function declareQueue(string $queueName): void
    {
        $url = 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName);
        $payload = json_encode([
            'durable' => true,
            'arguments' => ['x-queue-type' => 'quorum'],
        ]);

        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'PUT');
        curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        curl_exec($ch);
        curl_close($ch);
    }

    /**
     * Delete a queue via the RabbitMQ management API after tests.
     */
    protected function deleteQueue(string $queueName): void
    {
        $url = 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName);

        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
        curl_exec($ch);
        curl_close($ch);
    }
}
}
