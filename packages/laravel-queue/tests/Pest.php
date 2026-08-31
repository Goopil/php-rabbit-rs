<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Tests\TestCase;

require_once __DIR__.'/bootstrap.php';

uses(TestCase::class)->in(__DIR__);

class TestException extends \Exception {}

function liveConfig(string $queueName): array
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
                'tls' => ['enabled' => false],
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
                'delivery_limit' => null,
            ],
            'dead_letter' => null,
        ],
    ];
}

function uniqueQueue(string $prefix = 'rabbit-rs-it'): string
{
    return $prefix.'-'.uniqid('', true);
}

function declareQueue(string $queueName): void
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

function deleteQueue(string $queueName): void
{
    $url = 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName);

    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, 'DELETE');
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    curl_exec($ch);
    curl_close($ch);
}

/**
 * Extends the rabbit_rs user's configure permission so the native publisher
 * can lazily declare its internal exchanges (e.g. rabbit-rs.delayed).
 */
function grantRabbitRsConfigure(string $vhost = '/orders-eu'): void
{
    $url = 'http://localhost:15672/api/permissions/'.rawurlencode($vhost).'/rabbit_rs';
    $payload = json_encode([
        'configure' => '^(amq\\.|rabbit-rs-it-|rabbit-rs\\.)',
        'write' => '.*',
        'read' => '.*',
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
 * Provisions the delayed-message exchange and its binding to the queue.
 *
 * The native publisher declares the rabbit-rs.delayed exchange lazily but
 * does not bind queues to it: per the design doc's topology section, bindings
 * are provisioned by the infrastructure, so the harness provisions both the
 * exchange and the binding before delayed publishes.
 */
function provisionDelayedExchange(string $queueName, string $vhost = '/orders-eu'): void
{
    $encodedVhost = rawurlencode($vhost);
    $requests = [
        'PUT' => [
            'http://localhost:15672/api/exchanges/'.$encodedVhost.'/rabbit-rs.delayed' => json_encode([
                'type' => 'x-delayed-message',
                'durable' => true,
                'auto_delete' => false,
                'internal' => false,
                'arguments' => ['x-delayed-type' => 'direct'],
            ]),
        ],
        'POST' => [
            'http://localhost:15672/api/bindings/'.$encodedVhost.'/e/rabbit-rs.delayed/q/'.urlencode($queueName) => json_encode([
                'routing_key' => $queueName,
                'arguments' => [],
            ]),
        ],
    ];

    foreach ($requests as $method => $bodies) {
        foreach ($bodies as $url => $payload) {
            $ch = curl_init($url);
            curl_setopt($ch, CURLOPT_CUSTOMREQUEST, $method);
            curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
            curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
            curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
            curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
            curl_exec($ch);
            curl_close($ch);
        }
    }
}
