<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand;
use Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommandExtension;
use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Illuminate\Support\Facades\Log;

const CONSOLE_KERNEL = 'Illuminate\Contracts\Console\Kernel';

describe('rabbit-rs:work command', function () {
    it('command is registered', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();

        expect($commands)->toHaveKey('rabbit-rs:work');
    });

    it('command signature accepts workers and queue options', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();

        expect($definition->hasOption('workers'))->toBeTrue()
            ->and($definition->hasOption('queue'))->toBeTrue()
            ->and($definition->hasOption('connection'))->toBeTrue()
            ->and($definition->hasOption('max-restarts'))->toBeTrue()
            ->and($definition->hasOption('backoff'))->toBeTrue()
            ->and($definition->hasOption('rabbit-rs-worker'))->toBeTrue('--rabbit-rs-worker option should be recognized');
    });

    it('command signature accepts worker propagation options', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();

        expect($definition->hasOption('timeout'))->toBeTrue()
            ->and($definition->hasOption('tries'))->toBeTrue()
            ->and($definition->hasOption('memory'))->toBeTrue()
            ->and($definition->hasOption('max-jobs'))->toBeTrue()
            ->and($definition->hasOption('max-time'))->toBeTrue();
    });

    it('worker propagation options have expected defaults', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();

        expect($definition->getOption('timeout')->getDefault())->toBe('60')
            ->and($definition->getOption('memory')->getDefault())->toBe('128')
            ->and($definition->getOption('tries')->getDefault())->toBeNull()
            ->and($definition->getOption('max-jobs')->getDefault())->toBeNull()
            ->and($definition->getOption('max-time')->getDefault())->toBeNull();
    });

    it('default worker count is one', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();
        $workersOption = $definition->getOption('workers');

        expect($workersOption->getDefault())->toBe('1');
    });

    it('connection and queue default to null for fan-out', function () {
        $commands = $this->app->make(CONSOLE_KERNEL)->all();
        $command = $commands['rabbit-rs:work'];

        $definition = $command->getDefinition();

        expect($definition->getOption('connection')->getDefault())->toBeNull()
            ->and($definition->getOption('queue')->getDefault())->toBeNull();
    });
});

describe('rabbit-rs worker extension', function () {
    it('extension from environment returns null when no worker env set', function () {
        // Ensure the env var is not set in the test process.
        putenv(WorkerSupervisor::workerEnv());

        $extension = RabbitMqWorkCommandExtension::fromEnvironment();

        expect($extension->workerIndex())->toBeNull();
    });

    it('extension from environment returns index when worker env set', function () {
        putenv(WorkerSupervisor::workerEnv() . '=3');

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();

            expect($extension->workerIndex())->toBe(3);
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    });

    it('extension from option returns index when provided', function () {
        $extension = RabbitMqWorkCommandExtension::fromOption('5');

        expect($extension->workerIndex())->toBe(5);
    });

    it('extension from option returns null when empty', function () {
        expect(RabbitMqWorkCommandExtension::fromOption(null)->workerIndex())->toBeNull()
            ->and(RabbitMqWorkCommandExtension::fromOption('')->workerIndex())->toBeNull();
    });

    it('extension register is no-op when worker index is null', function () {
        putenv(WorkerSupervisor::workerEnv());

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();
            $called = false;
            $events = $this->app->make('events');
            $extension->register($events, static function () use (&$called): void {
                $called = true;
            });

            expect($called)->toBeFalse();
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    });

    it('extension register logs job processing event with worker tag', function () {
        putenv(WorkerSupervisor::workerEnv() . '=2');

        try {
            $extension = RabbitMqWorkCommandExtension::fromEnvironment();
            $logged = [];
            $events = $this->app->make('events');
            $extension->register($events, static function (string $level, array $context) use (&$logged): void {
                $logged[] = ['level' => $level, 'context' => $context];
            });

            // Build a mock job to dispatch a real JobProcessing event.
            $job = mockQueueJob();

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));

            // The extension should have logged the event with the worker tag.
            expect($logged)->not->toBeEmpty('JobProcessing event should have been logged');
            expect($logged[0]['level'])->toBe('info');
            expect($logged[0]['context']['worker'])->toBe('[worker-2]');
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    });
});

describe('rabbit-rs:work command handle wiring', function () {
    beforeEach(function (): void {
        // handle() resolves the work plan from config before creating the
        // supervisor: seed one rabbit-rs connection so the plan is non-empty.
        config()->set('queue.connections.rabbit-rs', [
            'driver' => 'rabbit-rs',
            'queue' => 'default',
            'hosts' => 'localhost:5672',
        ]);
    });

    /**
     * Verifies that the --rabbit-rs-worker CLI option is wired into the
     * command's handle() method: when the option is provided, the extension
     * is created via fromOption() and its listeners are registered so job
     * events are tagged with the worker index.
     *
     * A test-specific command subclass overrides createSupervisor() to return
     * a supervisor whose run() is a no-op, avoiding real child processes.
     */
    it('handle wires from option when rabbit-rs-worker option provided', function () {
        // Ensure the env var is not set so the extension only activates via
        // the CLI option, not via fromEnvironment().
        putenv(WorkerSupervisor::workerEnv());

        try {
            // Register a test command that stubs out the supervisor.
            registerTestWorkCommand($this->app);

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
            $job = mockQueueJob();

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));

            expect($logged)->not->toBeEmpty('JobProcessing event should have been logged via the extension wired in handle()');
            expect($logged[0]['context']['worker'])->toBe('[worker-2]');
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    });

    /**
     * Verifies that when --rabbit-rs-worker is not provided, handle() does
     * not register the extension and job events are not tagged.
     */
    it('handle does not register extension when rabbit-rs-worker option absent', function () {
        putenv(WorkerSupervisor::workerEnv());

        try {
            registerTestWorkCommand($this->app);

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
            $job = mockQueueJob();

            $events->dispatch(new \Illuminate\Queue\Events\JobProcessing('rabbit-rs', $job));
        } finally {
            putenv(WorkerSupervisor::workerEnv());
        }
    });
});

describe('rabbit-rs:work plan fan-out wiring', function () {
    beforeEach(function (): void {
        config()->set('queue.connections', [
            'eu' => [
                'driver' => 'rabbit-rs',
                'queue' => 'orders',
                'hosts' => 'eu-rabbit:5672',
            ],
            'us' => [
                'driver' => 'rabbit-rs',
                'queue' => 'orders',
                'hosts' => 'us-rabbit:5672',
            ],
        ]);
    });

    it('resolves a fan-out plan from the defaults and lists it', function () {
        $command = registerTestWorkCommand($this->app);

        $this->artisan('test:work-command')
            ->assertSuccessful()
            ->expectsOutputToContain('Starting 2 worker(s): eu[orders], us[orders]');

        expect($command->capturedPlan)->toBe([
            ['connection' => 'eu', 'queues' => ['orders']],
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });

    it('passes --connection/--queue filters into the plan', function () {
        $command = registerTestWorkCommand($this->app);

        $this->artisan('test:work-command', ['--connection' => 'us', '--queue' => 'orders'])
            ->assertSuccessful();

        expect($command->capturedPlan)->toBe([
            ['connection' => 'us', 'queues' => ['orders']],
        ]);
    });
});

/**
 * Register a test command that subclasses RabbitMqWorkCommand and stubs
 * out the supervisor so run() does not spawn real child processes.
 * Returns the registered command so tests can inspect the resolved plan.
 */
function registerTestWorkCommand($app): \Goopil\RabbitRs\Laravel\Console\RabbitMqWorkCommand
{
    $stubSupervisor = new class([['connection' => 'rabbit-rs', 'queues' => ['default']]], 1, 3, 1, null, []) extends WorkerSupervisor {
        public function run(): int
        {
            return WorkerSupervisor::EXIT_CLEAN;
        }
    };

    $command = new class($stubSupervisor) extends RabbitMqWorkCommand {
        public ?array $capturedPlan = null;

        protected $signature = 'test:work-command
            {--connection= : Comma-separated queue connections}
            {--queue= : Comma-separated queue names}
            {--workers=1 : Number of child workers}
            {--max-restarts=3 : Maximum restarts per worker}
            {--backoff=1 : Base backoff in seconds}
            {--timeout=60 : The number of seconds a child process can run}
            {--tries= : Number of times to attempt a job before failing it}
            {--memory=128 : The memory limit in megabytes}
            {--max-jobs= : The number of jobs to process before stopping}
            {--max-time= : The maximum number of seconds the worker should run}
            {--rabbit-rs-worker= : Worker index for logging/metrics attribution (set by the supervisor)}';

        protected $description = 'Test command';

        public function __construct(
            private readonly WorkerSupervisor $supervisor,
        ) {
            parent::__construct();
        }

        protected function createSupervisor(array $plan): WorkerSupervisor
        {
            $this->capturedPlan = $plan;

            return $this->supervisor;
        }
    };

    $app->make(CONSOLE_KERNEL)->registerCommand($command);

    return $command;
}

/**
 * Build a mock queue job with the standard expectations used to dispatch
 * a real JobProcessing event in the worker-tagging tests above.
 */
function mockQueueJob(): \Mockery\MockInterface
{
    $job = \Mockery::mock(\Illuminate\Contracts\Queue\Job::class);
    $job->shouldReceive('resolveName')->andReturn('TestJob');
    $job->shouldReceive('getJobId')->andReturn('test-123');
    $job->shouldReceive('getQueue')->andReturn('default');
    $job->shouldReceive('payload')->andReturn([]);
    $job->shouldReceive('uuid')->andReturn('test-uuid');
    $job->shouldReceive('attempts')->andReturn(1);
    $job->shouldReceive('getConnectionName')->andReturn('rabbit-rs');

    return $job;
}
