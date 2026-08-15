<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Symfony\Component\Process\Process;

final class WorkerSupervisor
{
    public const EXIT_CLEAN = 0;
    public const EXIT_SIGNAL = 130;
    public const EXIT_MAX_RESTARTS = 1;

    public function __construct(
        private readonly string $connection,
        private readonly string $queue,
        private readonly int $workers,
        private readonly int $maxRestarts,
        private readonly int $baseBackoffSeconds,
    ) {}

    /**
     * @return list<string>
     */
    public function buildChildCommand(int $workerIndex = 0): array
    {
        return [
            PHP_BINARY,
            'artisan',
            'queue:work',
            "--connection={$this->connection}",
            "--queue={$this->queue}",
            "--rabbit-rs-worker={$workerIndex}",
        ];
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
     */
    public function run(): int
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
                        return self::EXIT_MAX_RESTARTS;
                    }
                }
            }
            usleep(100_000);
        }

        foreach ($processes as $process) {
            if ($process->isRunning()) {
                $process->stop(10, SIGTERM);
            }
        }

        return self::EXIT_CLEAN;
    }

    private function startProcess(int $workerIndex): Process
    {
        $process = new Process($this->buildChildCommand($workerIndex));
        $process->start();

        return $process;
    }
}
