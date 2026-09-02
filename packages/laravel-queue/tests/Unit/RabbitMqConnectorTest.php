<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;

function connectorProperty(object $object, string $name): mixed
{
    // @phpstan-ignore-next-line — intentionally accessing private property for test verification.
    return (new ReflectionProperty($object, $name))->getValue($object);
}

beforeEach(function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-primary', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
    ]);
    $this->app['config']->set('queue.connections.rabbit-rs-secondary', [
        'driver' => 'rabbit-rs',
        'queue' => 'secondary',
    ]);

    (new class($this->app) extends RabbitMqServiceProvider {
        protected function nativeExtensionLoaded(): bool
        {
            return true;
        }
    })->boot();
});

it('resolves the RabbitMQ driver through the queue manager', function (): void {
    $queue = $this->app['queue']->connection('rabbit-rs-primary');

    expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
        ->and($queue->getConnectionName())->toBe('rabbit-rs-primary');
});

it('creates distinct pools for connections with different brokers', function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-eu', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'hosts' => 'eu-rabbit:5672',
    ]);
    $this->app['config']->set('queue.connections.rabbit-rs-us', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'hosts' => 'us-rabbit:5672',
    ]);

    $manager = $this->app['queue'];
    $euPool = connectorProperty($manager->connection('rabbit-rs-eu'), 'pool');
    $usPool = connectorProperty($manager->connection('rabbit-rs-us'), 'pool');

    expect($euPool)->not->toBe($usPool)
        ->and($euPool->config['brokers'][0]['name'])->toBe('rabbit-rs-eu')
        ->and($usPool->config['brokers'][0]['name'])->toBe('rabbit-rs-us');
});

it('shares one pool for connections with byte-identical configuration', function (): void {
    $config = ['driver' => 'rabbit-rs', 'queue' => 'default'];
    $this->app['config']->set('queue.connections.rabbit-rs-clone-a', $config);
    $this->app['config']->set('queue.connections.rabbit-rs-clone-b', $config);

    $manager = $this->app['queue'];
    $firstPool = connectorProperty($manager->connection('rabbit-rs-clone-a'), 'pool');
    $secondPool = connectorProperty($manager->connection('rabbit-rs-clone-b'), 'pool');

    expect($firstPool)->toBe($secondPool);
});

it('keeps distinct pools for connections that differ only in their queue', function (): void {
    $manager = $this->app['queue'];
    $primaryPool = connectorProperty($manager->connection('rabbit-rs-primary'), 'pool');
    $secondaryPool = connectorProperty($manager->connection('rabbit-rs-secondary'), 'pool');

    $primaryQueue = $manager->connection('rabbit-rs-primary');
    $secondaryQueue = $manager->connection('rabbit-rs-secondary');

    expect($primaryPool)->not->toBe($secondaryPool)
        ->and(connectorProperty($primaryQueue, 'defaultQueue'))->toBe('default')
        ->and(connectorProperty($secondaryQueue, 'defaultQueue'))->toBe('secondary');
});

it('does not reuse inherited pools after a fork', function (): void {
    $processId = 100;
    $factory = new NativePoolFactory(
        resolveProcessId: static function () use (&$processId): int {
            return $processId;
        },
    );
    $compiled = \Goopil\RabbitRs\Laravel\Config\ConnectionCompiler::compile(
        'rabbit-rs-primary',
        ['queue' => 'default'],
    );
    $parentPool = $factory->make($compiled['native']);

    $processId = 101;

    expect($factory->make($compiled['native']))->not->toBe($parentPool);
});

it('rejects an invalid default queue', function (): void {
    $connector = new RabbitMqConnector(
        new NativePoolFactory(),
    );

    $this->expectException(InvalidArgumentException::class);
    $this->expectExceptionMessage('queue');

    $connector->connect(['queue' => new stdClass()]);
});
