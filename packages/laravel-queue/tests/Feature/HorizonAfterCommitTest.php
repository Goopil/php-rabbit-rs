<?php

declare(strict_types=1);

use Fixtures\BulkJob;
use Fixtures\CommitJob;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;
use Illuminate\Support\Facades\DB;
use Illuminate\Support\Facades\Event;
use Laravel\Horizon\Events\JobPushed;

require_once __DIR__.'/../Fixture/horizon_jobs.php';

beforeEach(function (): void {
    $this->app['config']->set('database.connections.sqlite.database', ':memory:');

    $this->pool = new Pool();
    $this->app->singleton(NativePoolFactory::class, fn (): NativePoolFactory => new NativePoolFactory(
        createPool: fn (): Pool => $this->pool,
    ));

    bootFakeNativeExtension($this->app);

    $this->app['config']->set('queue.connections.rabbit-rs-horizon', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'horizon',
    ]);
});

it('defers Horizon job publication until the transaction commits', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    expect($queue)->toBeInstanceOf(RabbitMqQueue::class);

    $pool = $this->pool;

    DB::transaction(function () use ($queue, $pool) {
        dispatch(new CommitJob)->onConnection('rabbit-rs-horizon');
        expect($pool->published)->toBeEmpty('job must not be published inside the transaction');
    });

    expect($pool->published)->toHaveCount(1);
    $payload = json_decode($pool->published[0]['payload'], true, flags: JSON_THROW_ON_ERROR);
    expect($payload['type'])->toBe('job');
});

it('pushes Horizon bulk jobs with prepared payloads and events', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    Event::fake([JobPushed::class]);

    $queue->bulk([new BulkJob, new BulkJob], '', 'bulk');

    Event::assertDispatchedTimes(JobPushed::class, 2);
    expect($this->pool->publishedBatches)->toHaveCount(1);
    foreach ($this->pool->publishedBatches[0] as $message) {
        $payload = json_decode($message['payload'], true, flags: JSON_THROW_ON_ERROR);
        expect($payload['type'])->toBe('job')
            ->and($payload)->toHaveKey('tags')
            ->and($payload)->toHaveKey('pushedAt');
    }
});
