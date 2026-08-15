<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Tests\TestCase;

final class RabbitMqWorkCommandTest extends TestCase
{
    public function testCommandIsRegistered(): void
    {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();

        self::assertArrayHasKey('rabbit-rs:work', $commands);
    }

    public function testCommandSignatureAcceptsWorkersAndQueueOptions(): void
    {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();

        self::assertTrue($definition->hasOption('workers'));
        self::assertTrue($definition->hasOption('queue'));
        self::assertTrue($definition->hasOption('connection'));
        self::assertTrue($definition->hasOption('max-restarts'));
        self::assertTrue($definition->hasOption('backoff'));
    }

    public function testDefaultWorkerCountIsOne(): void
    {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();
        $workersOption = $definition->getOption('workers');

        self::assertSame('1', $workersOption->getDefault());
    }

    public function testDefaultConnectionIsRabbitRs(): void
    {
        $commands = $this->app->make('Illuminate\Contracts\Console\Kernel')->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();
        $connectionOption = $definition->getOption('connection');

        self::assertSame('rabbit-rs', $connectionOption->getDefault());
    }
}
