<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Laravel\Events\RabbitRsProbeEvaluated;
use Goopil\RabbitRs\Laravel\Support\ProbeStatefile;
use Illuminate\Console\Command;

/**
 * Kubernetes probes over the worker probe statefiles: exit 0 when healthy,
 * 1 otherwise. The statefile mtime is the consume-loop heartbeat; liveness
 * never inspects broker reachability — a broker outage is a readiness
 * concern, and failing liveness on it would restart-loop healthy workers.
 *
 * Aggregation: zero fresh statefiles fail; otherwise every fresh statefile
 * must satisfy the probe.
 */
final class RabbitMqProbeCommand extends Command
{
    private const PROBES = ['startup', 'ready', 'alive', 'prestop'];

    protected $signature = 'rabbit-rs:probe
        {probe : Probe to evaluate: startup, ready, alive or prestop}
        {--max-age=5 : Statefile freshness window in seconds}
        {--timeout=20 : prestop drain wait in seconds}';

    protected $description = 'Kubernetes probes over the Rabbit RS worker statefiles';

    public function handle(): int
    {
        $probe = (string) $this->argument('probe');
        if (! in_array($probe, self::PROBES, true)) {
            $this->error("Unknown probe '{$probe}'. Expected one of: ".implode(', ', self::PROBES));

            return self::FAILURE;
        }

        $directory = (string) config('rabbit-rs.probes.path', storage_path('framework/rabbit-rs/probes'));

        return $probe === 'prestop'
            ? $this->prestop($directory)
            : $this->evaluate($probe, $directory);
    }

    private function evaluate(string $probe, string $directory): int
    {
        $files = ProbeStatefile::fresh($directory, (float) $this->option('max-age'));
        $healthy = $files !== [] && match ($probe) {
            'ready' => ! in_array(false, array_column($files, 'connected'), true),
            'startup' => array_unique(array_column($files, 'state')) === ['running'],
            default => true, // alive: a fresh statefile is a turning consume loop
        };

        if ($files === []) {
            $state = 'no fresh statefile';
        } else {
            $state = sprintf('%d worker(s): %s', count($files), implode(', ', array_map(
                static fn (array $file): string => $file['pid'].':'.$file['state'].($file['connected'] ? '' : ', disconnected'),
                $files,
            )));
        }

        $event = new RabbitRsProbeEvaluated($probe, $state, $healthy);
        $this->laravel->make('events')->dispatch($event);
        $this->line($state);

        return $event->verdict && $healthy ? self::SUCCESS : self::FAILURE;
    }

    /**
     * Signals the fresh workers to drain and waits for them; always exits 0
     * because Kubernetes sends SIGTERM to the container regardless.
     *
     * Before signaling anyone, the hook marks the drain: each tracked
     * statefile carries `drain_requested` into its final draining write, and
     * the fleet marker file makes the supervisor defer recycling clean-exited
     * slots — without it, a SIGTERMed worker is instantly respawned under a
     * fresh PID (new statefile) while this hook still believes the fleet
     * drained. The markers are cleared after the bounded wait, whether or
     * not the drain completed: the wait is over and the supervisor must
     * resume recycling (the container's own SIGTERM stops the fleet right
     * after).
     */
    private function prestop(string $directory): int
    {
        $targets = ProbeStatefile::fresh($directory, (float) $this->option('max-age'));

        foreach ($targets as ['path' => $path]) {
            ProbeStatefile::requestDrain($path);
        }
        ProbeStatefile::signalDrain($directory);

        $this->signal($targets);

        $drained = $this->awaitDrain($directory, microtime(true) + (float) $this->option('timeout'));

        ProbeStatefile::clearDrainSignal($directory);
        foreach ($targets as ['path' => $path]) {
            ProbeStatefile::clearDrainRequest($path);
        }

        if ($targets === []) {
            $state = 'no fresh statefile';
        } else {
            $state = sprintf('%d worker(s) %s', count($targets), $drained ? 'drained' : 'did not drain in time');
        }

        $event = new RabbitRsProbeEvaluated('prestop', $state, $drained);
        $this->laravel->make('events')->dispatch($event);
        $this->line($state);

        return self::SUCCESS;
    }

    /**
     * @param  list<array{pid: int, state: string, connected: bool, consumed: int, acked: int, nacked: int, path: string}>  $targets
     */
    private function signal(array $targets): void
    {
        if ($targets === [] || ! function_exists('posix_kill')) {
            if ($targets !== []) {
                $this->warn('ext-posix unavailable: workers were not signaled, relying on the supervisor SIGTERM');
            }

            return;
        }
        $sigterm = defined('SIGTERM') ? constant('SIGTERM') : 15;
        foreach ($targets as ['pid' => $pid]) {
            @posix_kill($pid, $sigterm);
        }
    }

    /**
     * Polls the drain verdict until the fleet quiesces or the deadline
     * expires: the hook's wait stays bounded by the --timeout budget, never
     * an unbounded wait.
     */
    private function awaitDrain(string $directory, float $deadline): bool
    {
        while (true) {
            if ($this->drained($directory)) {
                return true;
            }
            if (microtime(true) >= $deadline) {
                return false;
            }
            usleep(100_000);
        }
    }

    /**
     * The fleet is quiesced: no fresh statefile reports a worker still
     * consuming. The scan is deliberately directory-wide: the original
     * targets age out of the freshness window once their workers exit, while
     * a respawned worker (fresh PID, new statefile) shows up as a fresh
     * booting/running file. Keying the verdict on the original targets alone
     * is what let the hook report success while respawned workers kept
     * consuming.
     */
    private function drained(string $directory): bool
    {
        foreach (ProbeStatefile::fresh($directory, (float) $this->option('max-age')) as ['state' => $state]) {
            if (! in_array($state, ['draining', 'stopped'], true)) {
                return false;
            }
        }

        return true;
    }
}
