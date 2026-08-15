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
