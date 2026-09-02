<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Illuminate\Console\Command;
use Illuminate\Contracts\Events\Dispatcher as EventDispatcher;
use Illuminate\Support\Facades\Log;

class RabbitMqWorkCommand extends Command
{
    protected $signature = 'rabbit-rs:work
        {--connection= : Comma-separated queue connections (default: every rabbit-rs connection)}
        {--queue= : Comma-separated queue names, resolved by definition (default: every defined queue)}
        {--workers=1 : Child workers per connection}
        {--max-restarts=3 : Maximum restarts per worker}
        {--backoff=1 : Base backoff in seconds}
        {--timeout=60 : The number of seconds a child process can run}
        {--tries= : Number of times to attempt a job before failing it}
        {--memory=128 : The memory limit in megabytes}
        {--max-jobs= : The number of jobs to process before stopping}
        {--max-time= : The maximum number of seconds the worker should run}
        {--rabbit-rs-worker= : Worker index for logging/metrics attribution (set by the supervisor)}';

    protected $description = 'Supervise Rabbit RS queue workers across connections with automatic restart';

    public function handle(): int
    {
        $this->registerWorkCommandExtension();

        $plan = WorkPlanResolver::resolve($this->option('connection'), $this->option('queue'));

        $supervisor = $this->createSupervisor($plan);

        $this->info(sprintf(
            'Starting %d worker(s): %s',
            count($plan) * (int) $this->option('workers'),
            $this->describePlan($plan),
        ));

        return $supervisor->run();
    }

    /**
     * Create the supervisor instance from the resolved work plan.
     *
     * Extracted as a protected method so tests can substitute a supervisor
     * that does not spawn real child processes.
     *
     * @param list<array{connection: string, queues: list<string>}> $plan
     */
    protected function createSupervisor(array $plan): WorkerSupervisor
    {
        $options = [
            'timeout'  => (int) $this->option('timeout'),
            'tries'    => $this->option('tries') !== null ? (int) $this->option('tries') : null,
            'memory'   => (int) $this->option('memory'),
            'max-jobs' => $this->option('max-jobs') !== null ? (int) $this->option('max-jobs') : null,
            'max-time' => $this->option('max-time') !== null ? (int) $this->option('max-time') : null,
        ];

        return new WorkerSupervisor(
            plan: $plan,
            workers: (int) $this->option('workers'),
            maxRestarts: (int) $this->option('max-restarts'),
            baseBackoffSeconds: (int) $this->option('backoff'),
            options: $options,
        );
    }

    /**
     * One-line plan description, e.g. "eu[orders, billing], us[orders]".
     *
     * @param list<array{connection: string, queues: list<string>}> $plan
     */
    private function describePlan(array $plan): string
    {
        $parts = [];
        foreach ($plan as ['connection' => $connection, 'queues' => $queues]) {
            $parts[] = $connection.'['.implode(', ', $queues).']';
        }

        return implode(', ', $parts);
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
