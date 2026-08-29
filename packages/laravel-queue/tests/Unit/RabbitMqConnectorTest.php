<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Http\Request;

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

it('shares the pool between equivalent queue connections without sharing their profile', function (): void {
    $primary = $this->app['queue']->connection('rabbit-rs-primary');
    $secondary = $this->app['queue']->connection('rabbit-rs-secondary');

    expect(connectorProperty($primary, 'pool'))->toBe(connectorProperty($secondary, 'pool'))
        ->and(connectorProperty($primary, 'defaultQueue'))->toBe('default')
        ->and(connectorProperty($secondary, 'defaultQueue'))->toBe('secondary');
});

it('shares the same pool for equivalent native configurations', function (): void {
    $factory = new NativePoolFactory();
    $config = ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs'))['native'];

    expect($factory->make($config))->toBe($factory->make($config));
});

it('creates different pools for different native configurations', function (): void {
    $factory = new NativePoolFactory();
    $firstConfig = ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs'))['native'];
    $secondConfig = $firstConfig;
    $secondConfig['brokers'][0]['heartbeat'] = 60;

    expect($factory->make($firstConfig))->not->toBe($factory->make($secondConfig));
});

it('does not reuse inherited pools after a fork', function (): void {
    $processId = 100;
    $factory = new NativePoolFactory(
        resolveProcessId: static function () use (&$processId): int {
            return $processId;
        },
    );
    $config = ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs'))['native'];
    $parentPool = $factory->make($config);

    $processId = 101;

    expect($factory->make($config))->not->toBe($parentPool);
});

it('does not retain request-scoped values in the connector', function (): void {
    $request = new Request();
    $reference = WeakReference::create($request);
    $connector = new RabbitMqConnector(
        new NativePoolFactory(),
        ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs')),
    );

    $connector->connect([
        'driver' => 'rabbit-rs',
        'request' => $request,
    ]);
    unset($request);
    gc_collect_cycles();

    expect($reference->get())->toBeNull();
});

it('rejects an invalid default queue', function (): void {
    $connector = new RabbitMqConnector(
        new NativePoolFactory(),
        ConfigNormalizer::normalize($this->app['config']->get('rabbit-rs')),
    );

    $this->expectException(InvalidArgumentException::class);
    $this->expectExceptionMessage('queue');

    $connector->connect(['queue' => new Request()]);
});
