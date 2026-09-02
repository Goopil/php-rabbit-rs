<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;

beforeEach(function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
    ]);

    bootFakeNativeExtension($this->app);
});

it('instantiates HorizonRabbitMqQueue when worker=horizon', function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-horizon', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'horizon',
    ]);

    $queue = $this->app['queue']->connection('rabbit-rs-horizon');

    expect($queue)->toBeInstanceOf(HorizonRabbitMqQueue::class)
        ->and($queue)->toBeInstanceOf(RabbitMqQueue::class);
});

it('instantiates RabbitMqQueue when worker=default', function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-default', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'default',
    ]);

    $queue = $this->app['queue']->connection('rabbit-rs-default');

    expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
        ->and($queue)->not->toBeInstanceOf(HorizonRabbitMqQueue::class);
});

it('instantiates RabbitMqQueue when worker is not set', function (): void {
    $queue = $this->app['queue']->connection('rabbit-rs');

    expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
        ->and($queue)->not->toBeInstanceOf(HorizonRabbitMqQueue::class);
});
