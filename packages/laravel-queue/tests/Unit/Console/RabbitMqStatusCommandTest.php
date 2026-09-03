<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;

const FAILED_TO_COLLECT_STATS = 'Failed to collect stats';

/**
 * Binds a NativePoolFactory whose pool creation always throws.
 */
function bindFailingPoolFactory($app): void
{
    $app->instance(NativePoolFactory::class, new NativePoolFactory(
        createPool: static function (): \Goopil\RabbitRs\Pool {
            throw new TestException('broker unreachable');
        },
    ));
}

beforeEach(function () {
    config()->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
    ]);
});

describe('RabbitMqStatusCommand exit codes', function () {
    it('returns FAILURE when stats collection throws', function () {
        bindFailingPoolFactory($this->app);

        $this->artisan('rabbit-rs:status')
            ->assertFailed()
            ->expectsOutputToContain(FAILED_TO_COLLECT_STATS);
    });

    it('returns SUCCESS when stats collection succeeds', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful();
    });

    it('returns FAILURE with json format when stats collection throws', function () {
        bindFailingPoolFactory($this->app);

        $this->artisan('rabbit-rs:status --format=json')
            ->assertFailed()
            ->expectsOutputToContain(FAILED_TO_COLLECT_STATS);
    });

    it('returns FAILURE when a connection fails to compile', function () {
        $this->app['config']->set('queue.connections.rabbit-rs.safety', 'bogus');

        $this->artisan('rabbit-rs:status')
            ->assertFailed()
            ->expectsOutputToContain(FAILED_TO_COLLECT_STATS);
    });

    it('returns FAILURE when no rabbit-rs connection is configured', function () {
        $this->app['config']->set('queue.connections.rabbit-rs', null);
        $this->app['config']->set('rabbit-rs', []);

        $this->artisan('rabbit-rs:status')
            ->assertFailed()
            ->expectsOutputToContain(FAILED_TO_COLLECT_STATS);
    });
});
