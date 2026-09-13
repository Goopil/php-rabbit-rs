<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Pool;

const FAILED_TO_COLLECT_STATS = 'Failed to collect stats';

/**
 * Binds a NativePoolFactory whose pool creation always throws.
 */
function bindFailingPoolFactory($app): void
{
    $app->instance(NativePoolFactory::class, new NativePoolFactory(
        createPool: static function (): Pool {
            throw new TestException('broker unreachable');
        },
    ));
}

/**
 * Binds a NativePoolFactory whose pool reports the default fake stats with
 * the dropped-publications counter forced to $droppedPublications.
 */
function bindDroppingPoolFactory($app, int $droppedPublications): void
{
    $app->instance(NativePoolFactory::class, new NativePoolFactory(
        createPool: static function () use ($droppedPublications): Pool {
            $pool = new Pool;
            $pool->statsResult = [...$pool->stats(), 'dropped_publications_total' => $droppedPublications];

            return $pool;
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

describe('RabbitMqStatusCommand drop counter (issue #290)', function () {
    it('reports the dropped publications counter in human output', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('dropped publications: 0');
    });

    it('warns when publications were dropped on a closed client', function () {
        bindDroppingPoolFactory($this->app, 3);

        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('dropped publications: 3')
            ->expectsOutputToContain('dropped_publications_total is 3');
    });
});
