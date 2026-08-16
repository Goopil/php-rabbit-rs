<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Illuminate\Contracts\Events\Dispatcher as EventDispatcher;
use Illuminate\Queue\Events\JobFailed;
use Illuminate\Queue\Events\JobProcessed;
use Illuminate\Queue\Events\JobProcessing;
use Illuminate\Queue\Events\JobReleasedAfterException;

/**
 * Extends Laravel's `queue:work` command with Rabbit RS worker identification.
 *
 * The supervisor spawns `queue:work` children and passes the worker index via
 * the `RABBIT_RS_WORKER` environment variable. This extension subscribes to
 * the queue job events and tags every log line with the worker index so that
 * logs and metrics from multiple supervised workers can be distinguished.
 *
 * The extension is registered by {@see RabbitMqServiceProvider::boot()} and is
 * only active when the `RABBIT_RS_WORKER` environment variable is set.
 */
final class RabbitMqWorkCommandExtension
{
    public function __construct(
        private readonly ?int $workerIndex = null,
    ) {}

    /**
     * Create an extension instance from the current environment.
     *
     * The worker index is read from the `RABBIT_RS_WORKER` environment variable
     * (set by the supervisor when spawning child processes).  When the env var
     * is absent, the extension is inactive.
     */
    public static function fromEnvironment(): self
    {
        $value = getenv(WorkerSupervisor::workerEnv());
        if ($value === false || $value === '') {
            return new self(null);
        }

        return new self((int) $value);
    }

    /**
     * Create an extension instance from a CLI option value.
     *
     * Used when the command is invoked directly with `--rabbit-rs-worker={i}`
     * rather than through the supervisor's env-var mechanism.
     */
    public static function fromOption(?string $value): self
    {
        if ($value === null || $value === '') {
            return new self(null);
        }

        return new self((int) $value);
    }

    /**
     * Returns the worker index, or null when the extension is inactive.
     */
    public function workerIndex(): ?int
    {
        return $this->workerIndex;
    }

    /**
     * Register the event listeners that augment worker output.
     *
     * @param  callable(string, array<string, mixed>): void  $logger
     */
    public function register(EventDispatcher $events, callable $logger): void
    {
        if ($this->workerIndex === null) {
            return;
        }

        $worker = $this->workerIndex;
        $prefix = "[worker-{$worker}]";

        $events->listen(JobProcessing::class, static function (JobProcessing $event) use ($logger, $prefix): void {
            $logger('info', [
                'worker' => $prefix,
                'status' => 'starting',
                'job' => $event->job->resolveName(),
                'job_id' => $event->job->getJobId(),
                'connection' => $event->connectionName,
                'queue' => $event->job->getQueue(),
            ]);
        });

        $events->listen(JobProcessed::class, static function (JobProcessed $event) use ($logger, $prefix): void {
            $logger('info', [
                'worker' => $prefix,
                'status' => 'processed',
                'job' => $event->job->resolveName(),
                'job_id' => $event->job->getJobId(),
                'connection' => $event->connectionName,
                'queue' => $event->job->getQueue(),
            ]);
        });

        $events->listen(JobReleasedAfterException::class, static function (JobReleasedAfterException $event) use ($logger, $prefix): void {
            $logger('warning', [
                'worker' => $prefix,
                'status' => 'released_after_exception',
                'job' => $event->job->resolveName(),
                'job_id' => $event->job->getJobId(),
                'connection' => $event->connectionName,
                'queue' => $event->job->getQueue(),
            ]);
        });

        $events->listen(JobFailed::class, static function (JobFailed $event) use ($logger, $prefix): void {
            $logger('error', [
                'worker' => $prefix,
                'status' => 'failed',
                'job' => $event->job->resolveName(),
                'job_id' => $event->job->getJobId(),
                'connection' => $event->connectionName,
                'queue' => $event->job->getQueue(),
                'exception' => $event->exception::class,
                'message' => $event->exception->getMessage(),
            ]);
        });
    }
}
