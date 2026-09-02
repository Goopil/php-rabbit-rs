<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Pool;

require_once __DIR__.'/bootstrap.php';

uses(TestCase::class)->in(__DIR__);

class TestException extends \Exception {}

/** Default test connection name: brokers and worker profiles compile under it. */
const INTEGRATION_CONNECTION = 'rabbit-rs-integration';

/** Default lab vhost the shared helpers operate on. */
const ORDERS_VHOST = '/orders-eu';

/**
 * Raw connection-shaped config (queue.connections.* entry) for the lab.
 */
function liveConfig(string $queueName): array
{
    return [
        'driver' => 'rabbit-rs',
        'queue' => $queueName,
        'hosts' => '127.0.0.1:5672',
        'vhost' => ORDERS_VHOST,
        'username' => 'rabbit_rs',
        'password' => 'rabbit_rs_lab',
        'exchange' => '',
        'routing_key' => '{queue}',
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

function declareQueue(string $queueName, string $vhost = ORDERS_VHOST): void
{
    managementRequest('PUT', 'http://localhost:15672/api/queues/'.rawurlencode($vhost).'/'.urlencode($queueName), json_encode([
        'durable' => true,
        'arguments' => ['x-queue-type' => 'quorum'],
    ]));
}

function deleteQueue(string $queueName, string $vhost = ORDERS_VHOST): void
{
    managementRequest('DELETE', 'http://localhost:15672/api/queues/'.rawurlencode($vhost).'/'.urlencode($queueName));
}

/**
 * Extends the rabbit_rs user's configure permission so the native publisher
 * can lazily declare its internal exchanges (e.g. rabbit-rs.delayed).
 */
function grantRabbitRsConfigure(string $vhost = ORDERS_VHOST): void
{
    managementRequest('PUT', 'http://localhost:15672/api/permissions/'.rawurlencode($vhost).'/rabbit_rs', json_encode([
        'configure' => '^(amq\\.|rabbit-rs-it-|rabbit-rs\\.)',
        'write' => '.*',
        'read' => '.*',
    ]));
}

/**
 * Builds the pool/queue pair integration tests drive, from the live lab
 * connection config run through the compiler. Returns [$pool, $queue]; the
 * caller owns pool cleanup (closePoolQuietly() for post-chaos teardown).
 *
 * $configOverrides patches the connection config (e.g. safety), and
 * $connectOverrides extends the connector options (e.g. block_for).
 * $brokerHosts replaces the default broker host list (e.g. routing the
 * connection through a per-test Toxiproxy proxy).
 */
function integrationPoolAndQueue(
    mixed $container,
    string $queueName,
    array $configOverrides = [],
    array $connectOverrides = [],
    string $connectionName = INTEGRATION_CONNECTION,
    ?array $brokerHosts = null,
): array {
    $config = array_merge(liveConfig($queueName), $configOverrides);
    if ($brokerHosts !== null) {
        $config['hosts'] = $brokerHosts;
    }
    $connectConfig = array_merge($config, $connectOverrides);

    // The connector resolves its compile-time name by reverse lookup in
    // queue.connections; register the exact connect config so the queue's
    // routes and worker profiles are named after this connection and match
    // the pool compiled below.
    $container['config']->set('queue.connections.'.$connectionName, $connectConfig);

    $compiled = ConnectionCompiler::compile($connectionName, $config);
    $pool = new Pool($compiled['native']);
    $factory = new NativePoolFactory(createPool: fn (): Pool => $pool);
    $queue = (new RabbitMqConnector($factory))->connect($connectConfig);
    $queue->setContainer($container);
    $queue->setConnectionName($connectionName);

    return [$pool, $queue];
}

