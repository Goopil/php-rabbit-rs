<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Support\ManagementApi;
use Illuminate\Console\Command;
use Illuminate\Contracts\Events\Dispatcher as EventDispatcher;
use InvalidArgumentException;

class RabbitMqWorkCommand extends Command
{
    protected $signature = 'rabbit-rs:work
        {--connection= : Comma-separated queue connections (default: every rabbit-rs connection)}
        {--queue= : Comma-separated queue names, resolved by definition (default: every defined queue)}
        {--workers=1 : Child workers per connection (the initial fleet when auto-scaling is configured)}
        {--max-restarts=3 : Maximum restarts per worker}
        {--backoff=1 : Base backoff in seconds}
        {--timeout=60 : The number of seconds a child process can run}
        {--tries= : Number of times to attempt a job before failing it}
        {--memory=128 : The memory limit in megabytes}
        {--max-jobs= : The number of jobs to process before stopping}
        {--max-time= : The maximum number of seconds the worker should run}
        {--stop-when-empty : Process pending jobs then exit once the children terminate (once mode for CI smoke tests; children get --stop-when-empty and are never recycled)}
        {--once : One-shot mode: children get --once (a single job each) and are never recycled; the supervisor exits once the fleet drains}
        {--min-workers=1 : Auto-scaling floor per connection}
        {--max-workers= : Auto-scaling ceiling per connection (default: fixed --workers fleet, scaling off)}
        {--scale-cooldown=3 : Minimum seconds between two scaling passes}
        {--scale-idle=30 : Seconds of continuous empty queues before releasing idle workers}
        {--rabbit-rs-worker= : Worker index for logging/metrics attribution (direct invocation only; the supervisor passes it via RABBIT_RS_WORKER_INDEX)}';

    protected $description = 'Supervise Rabbit RS queue workers across connections with automatic restart';

    public function handle(): int
    {
        $this->validateScalingOptions();

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
     * Validates the auto-scaling flags: --once and --stop-when-empty are two
     * mutually exclusive one-shot regimes, and the scaling ceiling must not
     * fall below the floor.
     *
     * @throws InvalidArgumentException
     */
    private function validateScalingOptions(): void
    {
        if ((bool) $this->option('once') && (bool) $this->option('stop-when-empty')) {
            throw new InvalidArgumentException('The --once and --stop-when-empty options are mutually exclusive: children cannot both stop after one job and drain until empty.');
        }

        $minWorkers = (int) $this->option('min-workers');
        $maxWorkers = $this->option('max-workers') !== null ? (int) $this->option('max-workers') : null;

        if ($maxWorkers !== null && $maxWorkers < $minWorkers) {
            throw new InvalidArgumentException(sprintf(
                'The --max-workers value (%d) must be greater than or equal to --min-workers (%d).',
                $maxWorkers,
                $minWorkers,
            ));
        }
    }

    /**
     * Create the supervisor instance from the resolved work plan.
     *
     * Extracted as a protected method so tests can substitute a supervisor
     * that does not spawn real child processes.
     *
     * @param  list<array{connection: string, queues: list<string>}>  $plan
     */
    protected function createSupervisor(array $plan): WorkerSupervisor
    {
        $options = [
            'timeout' => (int) $this->option('timeout'),
            'tries' => $this->option('tries') !== null ? (int) $this->option('tries') : null,
            'memory' => (int) $this->option('memory'),
            'max-jobs' => $this->option('max-jobs') !== null ? (int) $this->option('max-jobs') : null,
            'max-time' => $this->option('max-time') !== null ? (int) $this->option('max-time') : null,
            'stop-when-empty' => (bool) $this->option('stop-when-empty'),
        ];

        return new WorkerSupervisor(
            plan: $plan,
            workers: (int) $this->option('workers'),
            maxRestarts: (int) $this->option('max-restarts'),
            baseBackoffSeconds: (int) $this->option('backoff'),
            options: $options,
            minWorkers: (int) $this->option('min-workers'),
            maxWorkers: $this->option('max-workers') !== null ? (int) $this->option('max-workers') : null,
            scaleCooldownSeconds: (float) $this->option('scale-cooldown'),
            scaleIdleSeconds: (int) $this->option('scale-idle'),
            once: (bool) $this->option('once'),
            depthCallback: $this->depthCallback($plan),
        );
    }

    /**
     * Builds the supervisor's depth sampler: one call returns the ready
     * depth per plan connection, summed over the connection's planned
     * queues. Queues whose depth cannot be read (no management_url, failed
     * request) contribute nothing; a connection with no readable depth
     * reports null, which leaves it out of scaling silently.
     *
     * @param  list<array{connection: string, queues: list<string>}>  $plan
     * @return \Closure(): array<string, int|null>
     */
    private function depthCallback(array $plan): \Closure
    {
        return static function () use ($plan): array {
            $depths = [];
            foreach ($plan as $entry) {
                $depth = 0;
                $known = false;
                foreach ($entry['queues'] as $queue) {
                    $queueDepth = ManagementApi::queueDepth($entry['connection'], $queue);
                    if ($queueDepth !== null) {
                        $known = true;
                        $depth += $queueDepth;
                    }
                }

                $depths[$entry['connection']] = $known ? $depth : null;
            }

            return $depths;
        };
    }

    /**
     * One-line plan description, e.g. "eu[orders, billing], us[orders]".
     *
     * @param  list<array{connection: string, queues: list<string>}>  $plan
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

        /** @var EventDispatcher $events */
        $events = $this->laravel->make('events');
        $extension->registerWithLog($events);
    }
}
