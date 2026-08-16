<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommandExtension;
use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Illuminate\Support\Facades\Log;

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

    /**
     * Verifies that the --rabbit-rs-worker CLI option is wired into the
     * command's handle() method: when the option is provided, the extension
     * is created via fromOption() and its listeners are registered so job
     * events are tagged with the worker index.
     *
     * A test-specific command subclass overrides createSupervisor() to return
     * a supervisor whose run() is a no-op, avoiding real child processes.
     */
    public function testHandleWiresFromOptionWhenRabbitRsWorkerOptionProvided(): void
    {
        // Ensure the env var is not set so the extension only activates via
        // the CLI option, not via fromEnvironment().
        putenv(WorkerSupervisor::workerEnv());

        try {
            // Register a test command that stubs out the supervisor.
            $this->registerTestCommand();

            // Intercept Log::channel() calls to capture the worker tag.
            $logged = [];
            $logChannel = \Mockery::mock(\Psr\Log\LoggerInterface::class);
            $logChannel->shouldReceive('info')
                ->with('rabbit-rs worker', \Mockery::on(function ($context) use (&$logged): bool {
                    $logged[] = ['level' => 'info', 'context' => $context];

                    return true;
                }));
            $logManager = \Mockery::mock(\Illuminate\Log\LogManager::class);
            $logManager->shouldReceive('channel')->andReturn($logChannel);
            Log::swap($logManager);

            // Invoke the test command with --rabbit-rs-worker=2.
            $this->artisan('test:work-command', ['--rabbit-rs-worker' => '2'])
                ->assertSuccessful();

            // Dispatch a JobProcessing event; the listener registered by
            // handle() should log it with the [worker-2] tag.
            $events = $this->app->make('events');
            $job = \Mockery::mock(\Illuminate\Contracts\Queue\Job::class);
            $job->shouldReceive('resolveName')->andReturn('TestJob');
            $job->shouldReceive('getJobId')->andReturn('test-123');
            $job->shouldReceive('getQueue')->andReturn('default');
            $job->shouldReceive('payload')->andReturn([]);
            $job->shouldReceive('uuid')->andReturn('test-uuid');
            $job->shouldReceive('attempts')->andReturn(1);
            $job->shouldReceive('getConnectionName')->andReturn('rabbit-rs');

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));

            self::assertNotEmpty($logged, 'JobProcessing event should have been logged via the extension wired in handle()');
            self::assertSame('[worker-2]', $logged[0]['context']['worker']);
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }

    /**
     * Verifies that when --rabbit-rs-worker is not provided, handle() does
     * not register the extension and job events are not tagged.
     */
    public function testHandleDoesNotRegisterExtensionWhenRabbitRsWorkerOptionAbsent(): void
    {
        putenv(WorkerSupervisor::workerEnv());

        try {
            $this->registerTestCommand();

            $logChannel = \Mockery::mock(\Psr\Log\LoggerInterface::class);
            $logChannel->shouldNotReceive('info');
            $logManager = \Mockery::mock(\Illuminate\Log\LogManager::class);
            $logManager->shouldReceive('channel')->andReturn($logChannel);
            Log::swap($logManager);

            $this->artisan('test:work-command')
                ->assertSuccessful();

            // Dispatch a JobProcessing event; no listener should be registered
            // by handle() since the option was not provided.
            $events = $this->app->make('events');
            $job = \Mockery::mock(\Illuminate\Contracts\Queue\Job::class);
            $job->shouldReceive('resolveName')->andReturn('TestJob');
            $job->shouldReceive('getJobId')->andReturn('test-123');
            $job->shouldReceive('getQueue')->andReturn('default');
            $job->shouldReceive('payload')->andReturn([]);
            $job->shouldReceive('uuid')->andReturn('test-uuid');
            $job->shouldReceive('attempts')->andReturn(1);
            $job->shouldReceive('getConnectionName')->andReturn('rabbit-rs');

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    }

    /**
     * Register a test command that subclasses RabbitMqWorkCommand and stubs
     * out the supervisor so run() does not spawn real child processes.
     */
    private function registerTestCommand(): void
    {
        $stubSupervisor = new class('rabbit-rs', 'default', 1, 3, 1, null) extends WorkerSupervisor {
            public function run(): int
            {
                return WorkerSupervisor::EXIT_CLEAN;
            }
        };

        $command = new class($stubSupervisor) extends RabbitMqWorkCommand {
            protected $signature = 'test:work-command
                {--connection=rabbit-rs : The queue connection name}
                {--queue=default : The queue/profile name}
                {--workers=1 : Number of child workers}
                {--max-restarts=3 : Maximum restarts per worker}
                {--backoff=1 : Base backoff in seconds}
                {--rabbit-rs-worker= : Worker index for logging/metrics attribution (set by the supervisor)}';

            protected $description = 'Test command';

            public function __construct(
                private readonly WorkerSupervisor $supervisor,
            ) {
                parent::__construct();
            }

            protected function createSupervisor(): WorkerSupervisor
            {
                return $this->supervisor;
            }
        };

        $this->app->make('Illuminate\Contracts\Console\Kernel')->registerCommand($command);
    }
}
