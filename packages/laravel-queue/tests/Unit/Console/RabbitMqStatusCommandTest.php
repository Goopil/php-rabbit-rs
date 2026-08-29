<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;

describe('RabbitMqStatusCommand exit codes', function () {
    it('returns FAILURE when stats collection throws', function () {
        $factory = new NativePoolFactory(
            createPool: static function (): \Goopil\RabbitRs\Pool {
                throw new TestException('broker unreachable');
            },
        );

        $this->app->instance(NativePoolFactory::class, $factory);

        $this->artisan('rabbit-rs:status')
            ->assertFailed()
            ->expectsOutputToContain('Failed to collect stats');
    });

    it('returns SUCCESS when stats collection succeeds', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful();
    });

    it('returns FAILURE with json format when stats collection throws', function () {
        $factory = new NativePoolFactory(
            createPool: static function (): \Goopil\RabbitRs\Pool {
                throw new TestException('broker unreachable');
            },
        );

        $this->app->instance(NativePoolFactory::class, $factory);

        $this->artisan('rabbit-rs:status --format=json')
            ->assertFailed()
            ->expectsOutputToContain('Failed to collect stats');
    });

    it('returns FAILURE when config is invalid', function () {
        $this->app['config']->set('rabbit-rs', []);

        $this->artisan('rabbit-rs:status')
            ->assertFailed()
            ->expectsOutputToContain('Failed to collect stats');
    });
});
