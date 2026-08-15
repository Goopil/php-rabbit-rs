<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Illuminate\Console\Command;

final class RabbitMqWorkCommand extends Command
{
    protected $signature = 'rabbit-rs:work
        {--connection=rabbit-rs : The queue connection name}
        {--queue=default : The queue/profile name}
        {--workers=1 : Number of child workers}
        {--max-restarts=3 : Maximum restarts per worker}
        {--backoff=1 : Base backoff in seconds}';

    protected $description = 'Supervise multiple Rabbit RS queue workers with automatic restart';

    public function handle(): int
    {
        $supervisor = new WorkerSupervisor(
            connection: $this->option('connection'),
            queue: $this->option('queue'),
            workers: (int) $this->option('workers'),
            maxRestarts: (int) $this->option('max-restarts'),
            baseBackoffSeconds: (int) $this->option('backoff'),
        );

        $this->info("Starting {$supervisor->workers()} worker(s) on {$this->option('connection')}/{$this->option('queue')}");

        return $supervisor->run();
    }
}
