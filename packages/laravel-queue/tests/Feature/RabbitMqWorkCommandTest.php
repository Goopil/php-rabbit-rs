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
        self::assertTrue($definition->hasOption('rabbit-rs-worker'), '--rabbit-rs-worker option should be recognized');
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

    public function testExtensionFromOptionReturnsIndexWhenProvided(): void
    {
        $extension = RabbitMqWorkCommandExtension::fromOption('5');

        self::assertSame(5, $extension->workerIndex());
    }

    public function testExtensionFromOptionReturnsNullWhenEmpty(): void
    {
        self::assertNull(RabbitMqWorkCommandExtension::fromOption(null)->workerIndex());
        self::assertNull(RabbitMqWorkCommandExtension::fromOption('')->workerIndex());
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

            // Build a mock job to dispatch a real JobProcessing event.
            $job = \Mockery::mock(\Illuminate\Contracts\Queue\Job::class);
            $job->shouldReceive('resolveName')->andReturn('TestJob');
            $job->shouldReceive('getJobId')->andReturn('test-123');
            $job->shouldReceive('getQueue')->andReturn('default');
            $job->shouldReceive('payload')->andReturn([]);
            $job->shouldReceive('uuid')->andReturn('test-uuid');
            $job->shouldReceive('attempts')->andReturn(1);
            $job->shouldReceive('getConnectionName')->andReturn('rabbit-rs');

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));

            // The extension should have logged the event with the worker tag.
            self::assertNotEmpty($logged, 'JobProcessing event should have been logged');
            self::assertSame('info', $logged[0]['level']);
            self::assertSame('[worker-2]', $logged[0]['context']['worker']);
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }
}

