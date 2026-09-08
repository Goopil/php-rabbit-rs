<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Events\RabbitRsProbeEvaluated;
use Illuminate\Support\Facades\Event;

/**
 * Pids above any pid_max (macOS ~99998, Linux ≤ 4194304): prestop's
 * posix_kill fails with ESRCH instead of signaling a live dev-machine process.
 */
const PROBE_PID_A = 999999998;
const PROBE_PID_B = 999999999;

function writeProbeState(string $dir, int $pid, array $overrides = []): void
{
    @mkdir($dir, 0777, true);
    file_put_contents($dir.'/'.$pid.'.json', json_encode(array_merge([
        'pid' => $pid,
        'state' => 'running',
        'connected' => true,
        'consumed' => 0,
        'acked' => 0,
        'nacked' => 0,
    ], $overrides)));
}

beforeEach(function () {
    config()->set('rabbit-rs.probes.path', probeTempDir());
});

afterEach(function () {
    probeRmDir((string) config('rabbit-rs.probes.path'));
});

it('rejects an unknown probe name', function () {
    $this->artisan('rabbit-rs:probe', ['probe' => 'wat'])->assertExitCode(1);
});

it('passes alive with one fresh statefile even when the worker is disconnected', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A, ['connected' => false]);

    $this->artisan('rabbit-rs:probe', ['probe' => 'alive'])->assertExitCode(0);
});

it('fails alive when every statefile is stale', function () {
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A);
    touch($dir.'/'.PROBE_PID_A.'.json', time() - 10);

    $this->artisan('rabbit-rs:probe', ['probe' => 'alive', '--max-age' => '5'])->assertExitCode(1);
});

it('fails alive when no statefile exists at all', function () {
    $this->artisan('rabbit-rs:probe', ['probe' => 'alive'])->assertExitCode(1);
});

it('fails ready when a fresh worker is disconnected', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A, ['connected' => false]);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(1);
});

it('passes ready when every fresh worker is connected', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A);
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_B, ['consumed' => 7]);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(0);
});

it('fails ready on mixed fresh workers when one is disconnected', function () {
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A);
    writeProbeState($dir, PROBE_PID_B, ['connected' => false]);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(1);
});

it('ignores stale statefiles in the mixed scenario', function () {
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A);
    writeProbeState($dir, PROBE_PID_B, ['connected' => false]);
    touch($dir.'/'.PROBE_PID_B.'.json', time() - 10);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(0);
});

it('fails ready when no statefile exists', function () {
    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(1);
});

it('requires a fresh running statefile for startup', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A, ['state' => 'booting']);

    $this->artisan('rabbit-rs:probe', ['probe' => 'startup'])->assertExitCode(1);
});

it('passes startup once the worker is running', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A);

    $this->artisan('rabbit-rs:probe', ['probe' => 'startup'])->assertExitCode(0);
});

it('fails startup while one worker is still booting', function () {
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A);
    writeProbeState($dir, PROBE_PID_B, ['state' => 'booting']);

    $this->artisan('rabbit-rs:probe', ['probe' => 'startup'])->assertExitCode(1);
});

it('dispatches RabbitRsProbeEvaluated with the probe name and verdict', function () {
    Event::fake();
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(0);

    Event::assertDispatched(function (RabbitRsProbeEvaluated $event): bool {
        return $event->probe === 'ready' && $event->verdict === true;
    });
});

it('lets a listener force a healthy probe to fail', function () {
    Event::listen(RabbitRsProbeEvaluated::class, function (RabbitRsProbeEvaluated $event): void {
        $event->verdict = false;
    });
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A);

    $this->artisan('rabbit-rs:probe', ['probe' => 'ready'])->assertExitCode(1);
});

it('prestop signals workers, waits for the drain, and always exits zero', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A, ['state' => 'draining']);
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_B, ['state' => 'stopped']);

    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop', '--timeout' => '0.2'])->assertExitCode(0);
});

it('prestop exits zero when the drain does not complete in time', function () {
    writeProbeState(config('rabbit-rs.probes.path'), PROBE_PID_A, ['state' => 'running']);

    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop', '--timeout' => '0.2'])->assertExitCode(0);
});

it('prestop exits zero when no worker is fresh', function () {
    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop'])->assertExitCode(0);
});

it('dispatches the probe event for prestop too', function () {
    Event::fake();

    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop', '--timeout' => '0.1'])->assertExitCode(0);

    Event::assertDispatched(fn (RabbitRsProbeEvaluated $event): bool => $event->probe === 'prestop');
});
