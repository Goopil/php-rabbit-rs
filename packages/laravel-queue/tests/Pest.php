<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;

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

/**
 * Single call to the lab's RabbitMQ management API (admin credentials).
 * Returns the response body ('' on transport failure); callers that only
 * fire-and-forget (declare/delete) ignore it.
 */
function managementRequest(string $method, string $url, ?string $payload = null): string
{
    $ch = curl_init($url);
    curl_setopt($ch, CURLOPT_CUSTOMREQUEST, $method);
    curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
    curl_setopt($ch, CURLOPT_USERPWD, 'admin:admin_lab');
    if ($payload !== null) {
        curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
    }
    $body = curl_exec($ch);
    curl_close($ch);

    return $body === false ? '' : $body;
}

function declareQueue(string $queueName): void
{
    managementRequest('PUT', 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName), json_encode([
        'durable' => true,
        'arguments' => ['x-queue-type' => 'quorum'],
    ]));
}

function deleteQueue(string $queueName): void
{
    managementRequest('DELETE', 'http://localhost:15672/api/queues/%2Forders-eu/'.urlencode($queueName));
}

/**
 * Extends the rabbit_rs user's configure permission so the native publisher
 * can lazily declare its internal exchanges (e.g. rabbit-rs.delayed).
 */
function grantRabbitRsConfigure(string $vhost = '/orders-eu'): void
{
    managementRequest('PUT', 'http://localhost:15672/api/permissions/'.rawurlencode($vhost).'/rabbit_rs', json_encode([
        'configure' => '^(amq\\.|rabbit-rs-it-|rabbit-rs\\.)',
        'write' => '.*',
        'read' => '.*',
    ]));
}

/**
 * Builds the pool/queue pair integration tests drive, from the live lab
 * config run through the normalizer. Returns [$pool, $queue]; the caller owns
 * pool cleanup (closePoolQuietly() for post-chaos teardown).
 *
 * $configOverrides patches the live config (e.g. publisher confirms) and
 * $connectOverrides extends the connector options (e.g. block_for).
 * $brokerHosts replaces the default broker host list (e.g. routing the
 * connection through a per-test Toxiproxy proxy).
 */
function integrationPoolAndQueue(
    mixed $container,
    string $queueName,
    array $configOverrides = [],
    array $connectOverrides = [],
    string $connectionName = 'rabbit-rs-integration',
    ?array $brokerHosts = null,
): array {
    $config = array_merge(liveConfig($queueName), $configOverrides);
    if ($brokerHosts !== null) {
        $config['brokers']['default']['hosts'] = $brokerHosts;
    }
    $normalized = ConfigNormalizer::normalize($config);

    $pool = new Pool($normalized['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $pool);
    $queue = (new RabbitMqConnector($factory, $normalized))->connect(
        array_merge(['queue' => $queueName], $connectOverrides),
    );
    $queue->setContainer($container);
    $queue->setConnectionName($connectionName);

    return [$pool, $queue];
}
