<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Events\RabbitRsProbeEvaluated;
use Illuminate\Support\Facades\Event;
use Symfony\Component\Process\Process;

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

it('prestop does not report drained while a fresh untracked worker is still consuming', function () {
    // Supervisor recycling respawns a SIGTERMed worker under a fresh PID:
    // a NEW statefile the hook never tracked. The verdict must key on the
    // whole fleet's fresh statefiles, not the original targets alone —
    // keying on the targets is what let the hook report success while the
    // respawned worker kept consuming.
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A, ['state' => 'running']);

    // Mid-wait, the tracked worker flips to draining (its shutdown write)
    // and the recycled worker appears under a fresh PID.
    $midWait = new Process([PHP_BINARY, '-r', sprintf(
        'usleep(200000); file_put_contents(%s, json_encode(["pid" => %d, "state" => "draining", "connected" => true, "consumed" => 0, "acked" => 0, "nacked" => 0])); file_put_contents(%s, json_encode(["pid" => %d, "state" => "running", "connected" => true, "consumed" => 0, "acked" => 0, "nacked" => 0]));',
        var_export($dir.'/'.PROBE_PID_A.'.json', true),
        PROBE_PID_A,
        var_export($dir.'/'.PROBE_PID_B.'.json', true),
        PROBE_PID_B,
    )]);
    $midWait->start();

    Event::fake();
    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop', '--timeout' => '1'])->assertExitCode(0);
    $midWait->wait();

    Event::assertDispatched(function (RabbitRsProbeEvaluated $event): bool {
        return $event->probe === 'prestop' && $event->verdict === false;
    });
});

it('prestop marks the tracked worker and the fleet during the wait and clears both markers after', function () {
    $dir = (string) config('rabbit-rs.probes.path');
    writeProbeState($dir, PROBE_PID_A, ['state' => 'running']);

    // Observe the markers from a separate process while the hook is still
    // waiting (the worker stays running, so the wait spans the whole
    // timeout): the fleet marker must gate the supervisor, and the tracked
    // statefile must carry the drain flag. Both are cleared afterwards.
    $observation = sys_get_temp_dir().'/rabbit-rs-prestop-obs-'.uniqid('', true).'.json';
    $observer = new Process([PHP_BINARY, '-r', sprintf(
        'for ($i = 0; $i < 40; $i++) { if (is_file(%s)) { break; } usleep(20000); } $marker = is_file(%s); $data = json_decode((string) @file_get_contents(%s), true); $flag = is_array($data) && ($data["drain_requested"] ?? false) === true; file_put_contents(%s, json_encode(["marker" => $marker, "flag" => $flag]));',
        var_export($dir.'/drain.requested', true),
        var_export($dir.'/drain.requested', true),
        var_export($dir.'/'.PROBE_PID_A.'.json', true),
        var_export($observation, true),
    )]);
    $observer->start();

    $this->artisan('rabbit-rs:probe', ['probe' => 'prestop', '--timeout' => '1'])->assertExitCode(0);
    $observer->wait();

    $observed = json_decode((string) file_get_contents($observation), true);
    @unlink($observation);

    expect($observed['marker'])->toBeTrue('the fleet marker must exist while the hook waits')
        ->and($observed['flag'])->toBeTrue('the tracked statefile must carry the drain flag during the wait')
        ->and(is_file($dir.'/drain.requested'))->toBeFalse('the fleet marker must be cleared after the wait')
        ->and(json_decode((string) file_get_contents($dir.'/'.PROBE_PID_A.'.json'), true))
        ->not->toHaveKey('drain_requested');
});
