<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Exceptions\SupervisorException;
use Symfony\Component\Process\Process;

/**
 * @phpstan-type ProcessFactory \Closure(int): Process
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
     * @param ?ProcessFactory $processFactory Optional override used by tests
     *         to spawn a stub process instead of `queue:work`.
     * @param WorkerOptions $options Worker options to propagate to child processes.
     *         Keys: timeout, tries, memory, max-jobs, max-time. Null values are omitted.
     */
    public function __construct(
        private readonly string $connection,
        private readonly string $queue,
        private readonly int $workers,
        private readonly int $maxRestarts,
        private readonly int $baseBackoffSeconds,
        private readonly ?\Closure $processFactory = null,
        private readonly array $options = [],
    ) {}

    /**
     * Build the child command for the given worker index.
     *
     * The worker index is passed via the RABBIT_RS_WORKER environment variable
     * (see {@see workerEnv()}) rather than as a CLI option, because
     * `queue:work` is Laravel's built-in command and Symfony Console rejects
     * unknown options. The `--name` option (recognised by `queue:work`) is
     * set to a unique value so the worker name appears in logs and metrics.
     *
     * Worker options (timeout, tries, memory, max-jobs, max-time) are
     * propagated when set; null-valued options are omitted.
     *
     * @return list<string>
     */
    public function buildChildCommand(int $workerIndex = 0): array
    {
        $cmd = [
            PHP_BINARY,
            'artisan',
            'queue:work',
            "--connection={$this->connection}",
            "--queue={$this->queue}",
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

    public function workers(): int
    {
        return $this->workers;
    }

    public function maxRestarts(): int
    {
        return $this->maxRestarts;
    }

    /**
     * Starts the supervisor loop. Each child runs queue:work with the configured
     * connection and queue. On signal SIGTERM/SIGINT, children are stopped
     * gracefully. On unexpected exit, children are restarted with backoff
     * until maxRestarts is reached.
     *
     * @throws SupervisorException when ext-pcntl is not available
     */
    public function run(): int
    {
        if (! $this->canFork()) {
            throw new SupervisorException('ext-pcntl is required for the supervisor. Install it or run with --workers=1.');
        }

        return $this->runInternal();
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

    private function runInternal(): int
    {
        $processes = [];
        $restartCounts = array_fill(0, $this->workers, 0);

        $shutdown = false;
        $signalHandler = static function () use (&$shutdown): void {
            $shutdown = true;
        };

        pcntl_async_signals(true);
        pcntl_signal(SIGTERM, $signalHandler);
        pcntl_signal(SIGINT, $signalHandler);

        for ($i = 0; $i < $this->workers; $i++) {
            $processes[$i] = $this->startProcess($i);
        }

        while (! $shutdown) {
            foreach ($processes as $index => $process) {
                if (! $process->isRunning()) {
                    if ($this->shouldRestart($restartCounts[$index])) {
                        sleep($this->backoffSeconds($restartCounts[$index]));
                        $restartCounts[$index]++;
                        $processes[$index] = $this->startProcess($index);
                    } else {
                        $this->stopAllProcesses($processes);

                        return self::EXIT_MAX_RESTARTS;
                    }
                }
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

    private function startProcess(int $workerIndex): Process
    {
        if ($this->processFactory !== null) {
            $process = ($this->processFactory)($workerIndex);
        } else {
            $process = new Process(
                $this->buildChildCommand($workerIndex),
                null,
                $this->workerEnvironment($workerIndex),
            );
        }
        $process->start();

        return $process;
    }
}
