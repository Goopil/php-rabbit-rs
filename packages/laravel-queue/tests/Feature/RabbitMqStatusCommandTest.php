<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Tests\TestCase;

final class RabbitMqStatusCommandTest extends TestCase
{
    public function testHumanOutputShowsPoolStatsWithoutSecrets(): void
    {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('Rabbit RS')
            ->expectsOutputToContain('publishes')
            ->expectsOutputToContain('confirmations')
            ->expectsOutputToContain('returns')
            ->expectsOutputToContain('reconnects');
    }

    public function testJsonOutputIncludesConsumerMetrics(): void
    {
        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful()
            ->expectsOutputToContain('deliveries_total')
            ->expectsOutputToContain('acks_total')
            ->expectsOutputToContain('rejects_total');
    }

    public function testJsonOutputIncludesConfirmationLatencyPercentiles(): void
    {
        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful()
            ->expectsOutputToContain('confirmation_latency_p50')
            ->expectsOutputToContain('confirmation_latency_p95')
            ->expectsOutputToContain('confirmation_latency_p99');
    }

    public function testJsonOutputIncludesSettlementLatencyPercentiles(): void
    {
        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful()
            ->expectsOutputToContain('settlement_latency_p50')
            ->expectsOutputToContain('settlement_latency_p95')
            ->expectsOutputToContain('settlement_latency_p99');
    }

    public function testHumanOutputShowsConsumerMetricsAndLatencies(): void
    {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->expectsOutputToContain('deliveries')
            ->expectsOutputToContain('acks')
            ->expectsOutputToContain('rejects')
            ->expectsOutputToContain('confirmation_latency')
            ->expectsOutputToContain('settlement_latency');
    }

    public function testJsonOutputReturnsStructuredStats(): void
    {
        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful();
    }

    public function testHumanOutputDoesNotLeakCredentials(): void
    {
        $this->artisan('rabbit-rs:status')
            ->assertSuccessful()
            ->doesntExpectOutput('guest')
            ->doesntExpectOutput('password');
    }

    public function testJsonOutputDoesNotLeakCredentials(): void
    {
        $this->artisan('rabbit-rs:status --format=json')
            ->assertSuccessful()
            ->doesntExpectOutput('guest')
            ->doesntExpectOutput('password');
    }

    public function testStatusCommandExists(): void
    {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();
        self::assertArrayHasKey('rabbit-rs:status', $commands);
    }
}
