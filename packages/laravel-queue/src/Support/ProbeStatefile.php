<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

/**
 * Per-PID worker health statefile backing the rabbit-rs:probe Kubernetes
 * probes. The worker writes a small JSON document at every consume-loop turn
 * (throttled to the heartbeat window) and on every state transition; the file
 * mtime is the loop heartbeat.
 *
 * Statefiles are kept as post-mortem artifacts: a dead worker's file simply
 * ages out of the probe freshness window, and the writer sweeps files older
 * than an hour to bound directory growth under worker recycling.
 *
 * Every write is best-effort: probes must never break the worker.
 */
final class ProbeStatefile
{
    private const SWEEP_SECONDS = 3600;

    private ?float $lastWrite = null;

    private bool $started = false;

    private bool $draining = false;

    /** @var array{0: int, 1: int, 2: int} */
    private array $counters = [0, 0, 0];

    /** @var array<string, string> broker name => last connection state */
    private array $connectionStates = [];

    public function __construct(
        private readonly string $directory,
        private readonly int $pid,
        private readonly float $heartbeatSeconds = 1.0,
    ) {}

    public function due(): bool
    {
        return $this->lastWrite === null
            || (microtime(true) - $this->lastWrite) >= $this->heartbeatSeconds;
    }

    /**
     * Heartbeat for one consume-loop turn, carrying the current pool counters.
     */
    public function heartbeat(int $consumed, int $acked, int $nacked): void
    {
        $this->counters = [$consumed, $acked, $nacked];
        if ($this->due()) {
            $this->write();
        }
    }

    /**
     * Marks the first completed loop turn: the statefile flips booting → running.
     */
    public function markRunning(): void
    {
        if ($this->started) {
            return;
        }
        $this->started = true;
        $this->write();
    }

    /**
     * Records a native connection-state callback (ready, disconnected,
     * connecting, recovering): any state other than ready reports the worker
     * as disconnected. Writes immediately: this is a state transition.
     */
    public function recordConnectionState(string $broker, string $state): void
    {
        if (($this->connectionStates[$broker] ?? null) === $state) {
            return;
        }
        $this->connectionStates[$broker] = $state;
        $this->write();
    }

    /**
     * Flips the statefile to draining when the worker stops (queue:work fires
     * WorkerStopping on graceful shutdown and --max-jobs recycling).
     */
    public function draining(): void
    {
        if ($this->draining) {
            return;
        }
        $this->draining = true;
        $this->write();
    }

    /**
     * Reads and normalizes the statefiles fresher than $maxAgeSeconds.
     *
     * @return list<array{pid: int, state: string, connected: bool, consumed: int, acked: int, nacked: int, path: string}>
     */
    public static function fresh(string $directory, float $maxAgeSeconds): array
    {
        $files = [];
        $cutoff = time() - (int) ceil($maxAgeSeconds);
        foreach (glob($directory.'/*.json') ?: [] as $path) {
            $mtime = @filemtime($path);
            if ($mtime === false || $mtime < $cutoff) {
                continue;
            }
            $data = json_decode((string) @file_get_contents($path), true);
            if (! is_array($data)) {
                continue;
            }
            $files[] = [
                'pid' => (int) ($data['pid'] ?? 0),
                'state' => is_string($data['state'] ?? null) ? $data['state'] : '',
                'connected' => ($data['connected'] ?? false) === true,
                'consumed' => (int) ($data['consumed'] ?? 0),
                'acked' => (int) ($data['acked'] ?? 0),
                'nacked' => (int) ($data['nacked'] ?? 0),
                'path' => $path,
            ];
        }

        return $files;
    }

    private function state(): string
    {
        if ($this->draining) {
            return 'draining';
        }

        return $this->started ? 'running' : 'booting';
    }

    private function connected(): bool
    {
        foreach ($this->connectionStates as $state) {
            if ($state !== 'ready') {
                return false;
            }
        }

        return true;
    }

    private function write(): void
    {
        try {
            if (! is_dir($this->directory) && ! @mkdir($this->directory, 0775, true) && ! is_dir($this->directory)) {
                return;
            }
            $this->sweepAbandoned();
            $final = $this->directory.'/'.$this->pid.'.json';
            $tmp = $final.'.tmp';
            if (@file_put_contents($tmp, $this->payload()) === false) {
                return;
            }
            @rename($tmp, $final);
            $this->lastWrite = microtime(true);
        } catch (\Throwable) {
            // Probes are best-effort: never break the worker.
        }
    }

    private function payload(): string
    {
        [$consumed, $acked, $nacked] = $this->counters;

        return json_encode([
            'pid' => $this->pid,
            'state' => $this->state(),
            'connected' => $this->connected(),
            'consumed' => $consumed,
            'acked' => $acked,
            'nacked' => $nacked,
        ], JSON_THROW_ON_ERROR);
    }

    /**
     * Removes statefiles abandoned by dead workers: a live worker's heartbeat
     * keeps its file mtime fresh, so an hour-old file belongs to a gone pid.
     */
    private function sweepAbandoned(): void
    {
        $cutoff = time() - self::SWEEP_SECONDS;
        $own = $this->directory.'/'.$this->pid.'.json';
        foreach (glob($this->directory.'/*.json') ?: [] as $file) {
            $mtime = @filemtime($file);
            if ($file !== $own && $mtime !== false && $mtime < $cutoff) {
                @unlink($file);
            }
        }
    }
}
