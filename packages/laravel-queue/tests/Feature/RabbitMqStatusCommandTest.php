<?php

declare(strict_types=1);

const STATUS_JSON_COMMAND = 'rabbit-rs:status --format=json';

beforeEach(function () {
    config()->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
    ]);
});

describe('status command', function () {
    it('human output shows pool stats without secrets', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('Rabbit RS')
            ->expectsOutputToContain('publishes')
            ->expectsOutputToContain('confirmations')
            ->expectsOutputToContain('returns')
            ->expectsOutputToContain('reconnects');
    });

    it('json output includes consumer metrics', function () {
        $this->artisan(STATUS_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('deliveries_total')
            ->expectsOutputToContain('acks_total')
            ->expectsOutputToContain('rejects_total');
    });

    it('json output includes confirmation latency percentiles', function () {
        $this->artisan(STATUS_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('confirmation_latency_p50')
            ->expectsOutputToContain('confirmation_latency_p95')
            ->expectsOutputToContain('confirmation_latency_p99');
    });

    it('json output includes settlement latency percentiles', function () {
        $this->artisan(STATUS_JSON_COMMAND)
            ->assertSuccessful()
            ->expectsOutputToContain('settlement_latency_p50')
            ->expectsOutputToContain('settlement_latency_p95')
            ->expectsOutputToContain('settlement_latency_p99');
    });

    it('human output shows consumer metrics and latencies', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('deliveries')
            ->expectsOutputToContain('acks')
            ->expectsOutputToContain('rejects')
            ->expectsOutputToContain('confirmation_latency')
            ->expectsOutputToContain('settlement_latency');
    });

    it('json output returns structured stats', function () {
        $this->artisan(STATUS_JSON_COMMAND)
            ->assertSuccessful();
    });

    it('human output does not leak credentials', function () {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->doesntExpectOutput('guest')
            ->doesntExpectOutput('password');
    });

    it('json output does not leak credentials', function () {
        $this->artisan(STATUS_JSON_COMMAND)
            ->assertSuccessful()
            ->doesntExpectOutput('guest')
            ->doesntExpectOutput('password');
    });

    it('status command exists', function () {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();
        expect($commands)->toHaveKey('rabbit-rs:status');
    });
});
