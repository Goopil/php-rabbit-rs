<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Illuminate\Console\Command;
use Illuminate\Contracts\Events\Dispatcher as EventDispatcher;
use Illuminate\Support\Facades\Log;

class RabbitMqWorkCommand extends Command
{
    protected $signature = 'rabbit-rs:work
        {--connection=rabbit-rs : The queue connection name}
        {--queue=default : The queue/profile name}
        {--workers=1 : Number of child workers}
        {--max-restarts=3 : Maximum restarts per worker}
        {--backoff=1 : Base backoff in seconds}
        {--timeout=60 : The number of seconds a child process can run}
        {--tries= : Number of times to attempt a job before failing it}
        {--memory=128 : The memory limit in megabytes}
        {--max-jobs= : The number of jobs to process before stopping}
        {--max-time= : The maximum number of seconds the worker should run}
        {--rabbit-rs-worker= : Worker index for logging/metrics attribution (set by the supervisor)}';

    protected $description = 'Supervise multiple Rabbit RS queue workers with automatic restart';

    public function handle(): int
    {
        $this->registerWorkCommandExtension();

        $supervisor = $this->createSupervisor();

        $this->info("Starting {$supervisor->workers()} worker(s) on {$this->option('connection')}/{$this->option('queue')}");

        return $supervisor->run();
    }

    /**
     * Create the supervisor instance from the command options.
     *
     * Extracted as a protected method so tests can substitute a supervisor
     * that does not spawn real child processes.
     */
    protected function createSupervisor(): WorkerSupervisor
    {
        $options = [
            'timeout'  => (int) $this->option('timeout'),
            'tries'    => $this->option('tries') !== null ? (int) $this->option('tries') : null,
            'memory'   => (int) $this->option('memory'),
            'max-jobs' => $this->option('max-jobs') !== null ? (int) $this->option('max-jobs') : null,
            'max-time' => $this->option('max-time') !== null ? (int) $this->option('max-time') : null,
        ];

        return new WorkerSupervisor(
            connection: $this->option('connection'),
            queue: $this->option('queue'),
            workers: (int) $this->option('workers'),
            maxRestarts: (int) $this->option('max-restarts'),
            baseBackoffSeconds: (int) $this->option('backoff'),
            options: $options,
        );
    }

    /**
     * Register the work command extension when the --rabbit-rs-worker option is
     * provided, so that job events are tagged with the worker index in logs.
     *
     * When the command is invoked directly with `--rabbit-rs-worker={i}` (rather
     * than through the supervisor's env-var mechanism), this creates the
     * extension via {@see RabbitMqWorkCommandExtension::fromOption()} and
     * registers its event listeners on the application's event dispatcher.
     */
    private function registerWorkCommandExtension(): void
    {
        $extension = RabbitMqWorkCommandExtension::fromOption($this->option('rabbit-rs-worker'));

        if ($extension->workerIndex() === null) {
            return;
        }

        /** @var EventDispatcher $events */
        $events = $this->laravel->make('events');

        $extension->register($events, static function (string $level, array $context): void {
            Log::channel()->{$level}('rabbit-rs worker', $context);
        });
    }
}
