<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Exceptions\SupervisorException;
use Symfony\Component\Process\Process;

/**
 * @phpstan-type ProcessFactory \Closure(int): Process
 * @phpstan-type WorkPlanEntry array{connection: string, queues: list<string>}
 * @phpstan-type WorkerOptions array{timeout?: int|null, tries?: int|null, memory?: int|null, max-jobs?: int|null, max-time?: int|null}
 */
class WorkerSupervisor
{
    public const EXIT_CLEAN = 0;
    public const EXIT_SIGNAL = 130;
    public const EXIT_MAX_RESTARTS = 1;

    public const WORKER_ENV = 'RABBIT_RS_WORKER';

    /**
     * Worker options that are propagated to each child `queue:work` process.
     * Null values are omitted from the child command.
     */
    private const PROPAGATED_OPTIONS = ['timeout', 'tries', 'memory', 'max-jobs', 'max-time'];

    /**
     * @param list<WorkPlanEntry> $plan One entry per targeted connection; each
     *         child consumes one entry's queues via
     *         `queue:work --connection=<c> --queue=<q1,q2>`.
     * @param int $workers Children spawned per plan entry.
     * @param ?ProcessFactory $processFactory Optional override used by tests
     *         to spawn a stub process instead of `queue:work`.
     * @param WorkerOptions $options Worker options to propagate to child processes.
     *         Keys: timeout, tries, memory, max-jobs, max-time. Null values are omitted.
     */
    public function __construct(
        private readonly array $plan,
        private readonly int $workers,
        private readonly int $maxRestarts,
        private readonly int $baseBackoffSeconds,
        private readonly ?\Closure $processFactory = null,
        private readonly array $options = [],
    ) {}

    /**
     * Build one child command per plan entry × worker, with worker indexes
     * numbered across the full child list.
     *
     * The worker index is passed via the RABBIT_RS_WORKER environment variable
     * (see {@see workerEnvironment()}) rather than as a CLI option, because
     * `queue:work` is Laravel's built-in command and Symfony Console rejects
     * unknown options. The `--name` option (recognised by `queue:work`) is
     * set to a unique value so the worker name appears in logs and metrics.
     *
     * Worker options (timeout, tries, memory, max-jobs, max-time) are
     * propagated when set; null-valued options are omitted.
     *
     * @return list<list<string>>
     */
    public function buildChildCommands(): array
    {
        $commands = [];
        $index = 0;
        foreach ($this->plan as $entry) {
            for ($worker = 0; $worker < $this->workers; $worker++) {
                $commands[] = $this->childCommand($index, $entry);
                $index++;
            }
        }

        return $commands;
    }

    /**
     * @param WorkPlanEntry $entry
     * @return list<string>
     */
    private function childCommand(int $workerIndex, array $entry): array
    {
        $cmd = [
            PHP_BINARY,
            'artisan',
            'queue:work',
            "--connection={$entry['connection']}",
            '--queue='.implode(',', $entry['queues']),
            '--name=worker-'.$workerIndex,
        ];

        foreach (self::PROPAGATED_OPTIONS as $opt) {
            $value = $this->options[$opt] ?? null;
            if ($value !== null) {
                $cmd[] = "--{$opt}={$value}";
            }
        }

        return $cmd;
    }

    /**
     * Returns the environment variable name used to pass the worker index.
     */
    public static function workerEnv(): string
    {
        return self::WORKER_ENV;
    }

    /**
     * Returns the environment variables to set when spawning the given worker.
     *
     * @return array<string, string>
     */
    public function workerEnvironment(int $workerIndex): array
    {
        return [self::WORKER_ENV => (string) $workerIndex];
    }

    public function shouldRestart(int $currentRestarts): bool
    {
        return $currentRestarts < $this->maxRestarts;
    }

    public function backoffSeconds(int $currentRestarts): int
    {
        $seconds = $this->baseBackoffSeconds * (2 ** $currentRestarts);

        return min($seconds, 60);
    }

    /**
     * Starts the supervisor loop. Each child runs queue:work with its plan
     * entry's connection and queues. On signal SIGTERM/SIGINT, children are
     * stopped gracefully. A clean child exit (exit code 0, e.g. --max-jobs
     * recycling) restarts the child immediately and resets its crash budget;
     * a non-zero exit is a crash: the child is restarted with backoff until
     * maxRestarts is reached.
     *
     * When ext-pcntl is not available and a single child is configured, the
     * child runs in the foreground without forking ({@see runInline()}); no
     * pcntl function is needed on that path.
     *
     * @throws SupervisorException when ext-pcntl is not available and more
     *         than one child is configured
     */
    public function run(): int
    {
        $children = $this->buildChildCommands();

        if (! $this->canFork()) {
            if (count($children) === 1) {
                return $this->runInline($children);
            }

            throw new SupervisorException('ext-pcntl is required to supervise multiple workers. Install ext-pcntl or target a single connection.');
        }

        return $this->runInternal($children);
    }

    /**
     * Whether ext-pcntl is available for forking child processes.
     *
     * Overridden by test subclasses to simulate the absence of pcntl.
     */
    protected function canFork(): bool
    {
        return function_exists('pcntl_fork');
    }

    /**
     * Run a single child in the foreground without forking.
     *
     * Fallback for PHP builds without ext-pcntl (e.g. Windows): the child
     * process runs inline and the supervisor blocks until it exits, keeping
     * the same backoff and max-restarts semantics as the forking path.
     * Without pcntl there is no graceful signal handling: the default signal
     * disposition terminates the supervisor, leaving the child to stop on
     * its own.
     *
     * @param list<list<string>> $children Exactly one child command.
     */
    private function runInline(array $children): int
    {
        $restarts = 0;
        $process = $this->startProcess(0, $children[0]);

        while (true) {
            $process->wait();

            if ($this->isCleanExit($process)) {
                // Planned recycling (e.g. --max-jobs reached): reset the
                // crash budget and restart immediately, without backoff.
                $restarts = 0;
                $process = $this->startProcess(0, $children[0]);

                continue;
            }

            if (! $this->shouldRestart($restarts)) {
                return self::EXIT_MAX_RESTARTS;
            }

            sleep($this->backoffSeconds($restarts));
            $restarts++;
            $process = $this->startProcess(0, $children[0]);
        }
    }

    /**
     * Whether the child exited cleanly (planned recycling, e.g. --max-jobs
     * or --max-time reached): exit code 0. A clean exit resets the crash
     * budget and restarts immediately; only non-zero exits are treated as
     * crashes and consume the restart budget with backoff.
     */
    private function isCleanExit(Process $process): bool
    {
        return $process->getExitCode() === self::EXIT_CLEAN;
    }

    /**
     * @param list<list<string>> $children One command per child process,
     *         indexed by worker index.
     */
    private function runInternal(array $children): int
    {
        $processes = [];
        $restartCounts = array_fill(0, count($children), 0);
        // Next allowed restart time per worker (unix timestamp seconds).
        // 0.0 means no restart is pending: the value is set once when a dead
        // worker is detected, and consumed once the backoff window elapsed.
        // While a worker waits out its backoff, the loop keeps polling the
        // other children (non-blocking backoff).
        $restartAt = array_fill(0, count($children), 0.0);

        $shutdown = false;
        $signalHandler = static function () use (&$shutdown): void {
            $shutdown = true;
        };

        pcntl_async_signals(true);
        pcntl_signal(SIGTERM, $signalHandler);
        pcntl_signal(SIGINT, $signalHandler);

        foreach ($children as $index => $command) {
            $processes[$index] = $this->startProcess($index, $command);
        }

        while (! $shutdown) {
            $now = microtime(true);
            foreach ($processes as $index => $process) {
                if ($process->isRunning()) {
                    continue;
                }

                if ($this->isCleanExit($process)) {
                    // Planned recycling (e.g. --max-jobs reached): reset the
                    // crash budget and restart immediately, without backoff.
                    $restartCounts[$index] = 0;
                    $processes[$index] = $this->startProcess($index, $children[$index]);

                    continue;
                }

                if ($restartAt[$index] !== 0.0) {
                    // A restart is already scheduled for this worker: wait for
                    // its backoff window to elapse, then restart it. The other
                    // children keep being supervised in the meantime.
                    if ($now >= $restartAt[$index]) {
                        $restartAt[$index] = 0.0;
                        $processes[$index] = $this->startProcess($index, $children[$index]);
                    }
                    continue;
                }

                if (! $this->shouldRestart($restartCounts[$index])) {
                    $this->stopAllProcesses($processes);

                    return self::EXIT_MAX_RESTARTS;
                }

                // Schedule the restart with its backoff; the loop keeps
                // polling the other children meanwhile (non-blocking backoff).
                $restartAt[$index] = $now + $this->backoffSeconds($restartCounts[$index]);
                $restartCounts[$index]++;
            }
            usleep(100_000);
        }

        $this->stopAllProcesses($processes);

        return self::EXIT_CLEAN;
    }

    /**
     * Stop all running child processes gracefully.
     *
     * @param array<int, Process> $processes
     */
    private function stopAllProcesses(array $processes): void
    {
        foreach ($processes as $process) {
            if ($process->isRunning()) {
                $process->stop(10, SIGTERM);
            }
        }
    }

    /**
     * @param list<string> $command The child command for this worker index.
     */
    private function startProcess(int $workerIndex, array $command): Process
    {
        if ($this->processFactory !== null) {
            $process = ($this->processFactory)($workerIndex);
        } else {
            $process = new Process(
                $command,
                null,
                $this->workerEnvironment($workerIndex),
            );
        }
        $process->start();

        return $process;
    }
}
