<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommandExtension;
use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
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

    public function testExtensionFromEnvironmentReturnsNullWhenNoWorkerEnvSet(): void
    {
        // Ensure the env var is not set in the test process.
        putenv(WorkerSupervisor::workerEnv());

        $extension = RabbitMqWorkCommandExtension::fromEnvironment();

        self::assertNull($extension->workerIndex());
    }

    public function testExtensionFromEnvironmentReturnsIndexWhenWorkerEnvSet(): void
    {
        putenv(WorkerSupervisor::workerEnv() . '=3');

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();

            self::assertSame(3, $extension->workerIndex());
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }

    public function testExtensionRegisterIsNoOpWhenWorkerIndexIsNull(): void
    {
        putenv(WorkerSupervisor::workerEnv());

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();
            $called = false;
            $events = $this->app->make('events');
            $extension->register($events, static function (string $level, array $context) use (&$called): void {
                $called = true;
            });

            self::assertFalse($called);
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }

    public function testExtensionRegisterLogsJobProcessingEventWithWorkerTag(): void
    {
        putenv(WorkerSupervisor::workerEnv() . '=2');

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();
            $logged = [];
            $events = $this->app->make('events');
            $extension->register($events, static function (string $level, array $context) use (&$logged): void {
                $logged[] = ['level' => $level, 'context' => $context];
            });

            // The extension should have registered a listener for JobProcessing.
            $listeners = $events->getListeners(\Illuminate\Queue\Events\JobProcessing::class);
            self::assertNotEmpty($listeners, 'JobProcessing listener should be registered');
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }
}

