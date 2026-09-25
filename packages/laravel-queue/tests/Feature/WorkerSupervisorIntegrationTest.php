<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Goopil\RabbitRs\Laravel\Support\QueueDepthSampler;
use Illuminate\Support\Facades\Log;
use Symfony\Component\Process\Process;

const WORKER_STUB_PATH = '/Fixture/worker_stub.php';

describe('WorkerSupervisor integration', function () {
    beforeEach(function () {
        $this->stateDir = sys_get_temp_dir().'/rabbit-rs-supervisor-'.uniqid('', true);
        @mkdir($this->stateDir, 0o777, true);
    });

    afterEach(function () {
        supervisorCleanupStateDir($this->stateDir);
    });

    it('spawned worker receives worker index via environment', function () {
        // Crash mode keeps the run bounded: a clean-exiting child now recycles
        // indefinitely, so the supervisor would never return on its own.
        $supervisor = makeSupervisor(workers: 1, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'crash',
        ]);

        $supervisor->run();

        // The marker file records the worker index received by the child.
        $marker = supervisorWaitForMarker(0);
        expect($marker)->not->toBeNull('Worker 0 should have started and written a marker');
        expect($marker['worker'])->toBe(0);
    });

    it('multiple workers each receive distinct index', function () {
        $supervisor = makeSupervisor(workers: 2, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'crash',
        ]);

        $supervisor->run();

        $marker0 = supervisorWaitForMarker(0);
        $marker1 = supervisorWaitForMarker(1);
        expect($marker0)->not->toBeNull('Worker 0 should have started');
        expect($marker1)->not->toBeNull('Worker 1 should have started');
        expect($marker0['worker'])->toBe(0);
        expect($marker1['worker'])->toBe(1);
    });

    it('crashed worker is restarted with backoff', function () {
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash'],
        );

        $exit = $supervisor->run();

        // Each crash is a non-zero exit; the supervisor restarts until maxRestarts.
        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);
        expect(supervisorInvocationCount(0))->toBeGreaterThan(1);
    });

    it('max restarts reached returns exit max restarts', function () {
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 2,
            baseBackoffSeconds: 0,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash'],
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);
        // Initial + maxRestarts attempts.
        expect(supervisorInvocationCount(0))->toBe(1 + 2);
    });

    it('clean exits do not burn the restart budget', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        // Five clean cycles (more than maxRestarts=3) followed by crashes:
        // clean recycling must reset the budget each time, then crash
        // protection must still trip with a fresh budget.
        $modes = ['exit-clean', 'exit-clean', 'exit-clean', 'exit-clean', 'exit-clean', 'crash', 'crash', 'crash', 'crash'];
        $calls = 0;
        $factory = static function () use (&$calls, $modes, $stubPath, $stateDir): Process {
            $mode = $modes[$calls] ?? 'crash';
            $calls++;

            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => '0',
                'RABBIT_RS_STUB_MODE' => $mode,
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: $factory,
        );

        $exit = $supervisor->run();

        // 5 clean recycles + initial crash + 3 crash restarts, then stop.
        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS)
            ->and($calls)->toBe(9);
    });

    it('restarts clean exits immediately without waiting out backoff', function () {
        // Run the supervisor in a subprocess: with clean recycling it would
        // otherwise never return on its own.
        $script = writeSupervisorScript(mode: 'exit-clean', maxRestarts: 3, baseBackoffSeconds: 2);
        $process = new Process([PHP_BINARY, $script, test()->stateDir]);
        $process->start();

        // Five clean cycles must complete quickly. With the bug (clean exits
        // burn the budget) the fleet stops after 4 cycles; with backoff on
        // clean restarts the 5th start would land at >= 30s.
        $deadline = microtime(true) + 3.0;
        $survived = false;
        while (microtime(true) < $deadline) {
            if (supervisorInvocationCount(0) >= 5) {
                $survived = true;
                break;
            }
            usleep(20_000);
        }

        expect($survived)->toBeTrue('worker should survive past max-restarts clean cycles without backoff');

        $supervisorPid = $process->getPid();
        expect($supervisorPid)->not->toBeNull();
        posix_kill($supervisorPid, SIGTERM);

        $process->wait();

        expect($process->getExitCode())->toBe(WorkerSupervisor::EXIT_CLEAN);
    });

    it('recycles clean exits inline without pcntl and without burning the budget', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $modes = ['exit-clean', 'exit-clean', 'exit-clean', 'exit-clean', 'exit-clean', 'crash', 'crash'];
        $calls = 0;
        $factory = static function () use (&$calls, $modes, $stubPath, $stateDir): Process {
            $mode = $modes[$calls] ?? 'crash';
            $calls++;

            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => '0',
                'RABBIT_RS_STUB_MODE' => $mode,
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        // Simulate a PHP build without ext-pcntl (the class exposes the hook for tests).
        $supervisor = new class(plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]], workers: 1, maxRestarts: 1, baseBackoffSeconds: 0, processFactory: $factory) extends WorkerSupervisor
        {
            protected function canFork(): bool
            {
                return false;
            }
        };

        $exit = $supervisor->run();

        // 5 clean recycles (budget reset each time) + initial crash + 1 crash restart.
        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS)
            ->and($calls)->toBe(7);
    });

    it('clean-exiting worker keeps recycling while another worker crash-loops', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $starts = [0 => 0, 1 => 0];
        $factory = static function (int $workerIndex) use (&$starts, $stubPath, $stateDir): Process {
            $starts[$workerIndex]++;

            if ($workerIndex === 0) {
                // Crash-loops: each run dies non-zero after ~2s. The window
                // must leave room for several clean recycles of worker 1
                // even on a loaded machine, where one recycle can take
                // ~0.5s (poll tick + PHP cold start).
                return new Process([PHP_BINARY, '-r', 'usleep(2000000); exit(1);'], null, [
                    'RABBIT_RS_WORKER_INDEX' => '0',
                ]);
            }

            // Recycles cleanly every few hundred milliseconds.
            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => '1',
                'RABBIT_RS_STUB_MODE' => 'exit-clean',
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            processFactory: $factory,
        );

        $exit = $supervisor->run();

        // Crash protection intact: worker 0 trips max restarts...
        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS)
            // ...with exactly its budget (initial + 1 restart)...
            ->and($starts[0])->toBe(2)
            // ...while the clean worker recycled far beyond the crash budget.
            // Assert on the parent-side spawn counter: it is deterministic,
            // and a restart beyond the first requires the previous child to
            // have exited cleanly (worker 1's crash budget of 1 caps
            // crash-driven starts at 2), so 5 starts prove sustained clean
            // recycling. The child-written count is only a secondary
            // observation: the final stop signal can cut its last in-flight
            // write short, so it only asserts the child actually executed.
            ->and($starts[1])->toBeGreaterThanOrEqual(5)
            ->and(supervisorInvocationCount(1))->toBeGreaterThanOrEqual(1);
    });

    it('stops all children when one worker exceeds max restarts', function () {
        // Worker 0 crashes immediately (exhausting max-restarts quickly).
        // Worker 1 runs until signaled ("run" mode).
        // The supervisor must stop worker 1 before returning EXIT_MAX_RESTARTS.
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $factory = static function (int $workerIndex) use ($stubPath, $stateDir): Process {
            $cmd = [PHP_BINARY, $stubPath];
            $mode = $workerIndex === 0 ? 'crash' : 'run';
            $envForChild = [
                'RABBIT_RS_WORKER_INDEX' => (string) $workerIndex,
                'RABBIT_RS_STUB_MODE' => $mode,
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ];

            return new Process($cmd, null, $envForChild);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            processFactory: $factory,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);

        // Worker 0 should have been started and crashed.
        expect(supervisorInvocationCount(0))->toBeGreaterThanOrEqual(1);

        // Worker 1 should have been started and then stopped (not still running).
        // The marker file proves it started; the exit file proves it was stopped.
        $marker = supervisorWaitForMarker(1, timeoutMs: 1000);
        expect($marker)->not->toBeNull('Worker 1 should have started');

        $exitFile = $stateDir.'/worker-1-exited.txt';
        $deadline = microtime(true) + 2.0;
        while (microtime(true) < $deadline) {
            if (is_file($exitFile)) {
                break;
            }
            usleep(20_000);
        }
        expect(is_file($exitFile))->toBeTrue('Worker 1 should have been stopped by the supervisor, not left as an orphan');
    });

    it('runs a single worker inline without pcntl', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $factory = static function (int $workerIndex) use ($stubPath, $stateDir): Process {
            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => (string) $workerIndex,
                'RABBIT_RS_STUB_MODE' => 'crash',
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        // Simulate a PHP build without ext-pcntl (the class exposes the hook for tests).
        $supervisor = new class(plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]], workers: 1, maxRestarts: 1, baseBackoffSeconds: 0, processFactory: $factory) extends WorkerSupervisor
        {
            protected function canFork(): bool
            {
                return false;
            }
        };

        $exit = $supervisor->run();

        // Must not throw a SupervisorException: with a single worker the child
        // runs in the foreground, no forking and no pcntl involved. The restart
        // semantics mirror the forking path (initial start + one restart).
        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);

        $marker = supervisorWaitForMarker(0);
        expect($marker)->not->toBeNull('Worker 0 should have run inline');
        expect($marker['worker'])->toBe(0);
        expect(supervisorInvocationCount(0))->toBe(2);
    });

    it('keeps supervising other children while one is in backoff', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $starts = [0 => [], 1 => []];
        $factory = static function (int $workerIndex) use (&$starts, $stubPath, $stateDir): Process {
            $starts[$workerIndex][] = microtime(true);

            if ($workerIndex === 0) {
                // Crashes immediately on every start.
                return new Process([PHP_BINARY, $stubPath], null, [
                    'RABBIT_RS_WORKER_INDEX' => '0',
                    'RABBIT_RS_STUB_MODE' => 'crash',
                    'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
                ]);
            }

            // Stays up for a moment, then exits non-zero: its crash lands well
            // inside worker 0's backoff window.
            return new Process([PHP_BINARY, '-r', 'usleep(500000); exit(1);'], null, [
                'RABBIT_RS_WORKER_INDEX' => '1',
            ]);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 2,
            maxRestarts: 2,
            baseBackoffSeconds: 3,
            processFactory: $factory,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);
        expect(count($starts[0]))->toBe(3);
        expect(count($starts[1]))->toBe(2);

        // Worker 1's first restart must not be serialised behind worker 0's
        // backoff window: a blocking sleep() would delay it by a full backoff
        // period (>= 3s gap). Non-blocking polling keeps the gap under 1.5s.
        $gap = $starts[1][1] - $starts[0][1];
        expect($gap)->toBeLessThan(1.5);
    });

    it('run mode worker then signal returns clean exit', function () {
        // Run the supervisor in a subprocess so we can send it a signal.
        $script = writeSupervisorScript();
        $process = new Process([PHP_BINARY, $script, test()->stateDir]);
        $process->start();

        // Wait for the worker to start.
        $marker = null;
        $deadline = microtime(true) + 5.0;
        while (microtime(true) < $deadline) {
            $marker = supervisorWaitForMarker(0, timeoutMs: 100);
            if ($marker !== null) {
                break;
            }
        }
        expect($marker)->not->toBeNull('Worker should have started before sending SIGTERM');

        usleep(100_000); // Give the supervisor a moment to enter its loop.

        $supervisorPid = $process->getPid();
        expect($supervisorPid)->not->toBeNull();
        posix_kill($supervisorPid, SIGTERM);

        $process->wait();
        $exitCode = $process->getExitCode();

        expect($exitCode)->toBe(WorkerSupervisor::EXIT_CLEAN);
    });

    it('stop-when-empty exits once children terminate, without restarting them', function () {
        $calls = [0 => 0, 1 => 0];
        $supervisor = makeSupervisor(
            workers: 2,
            maxRestarts: 3,
            modes: [0 => 'exit-clean', 1 => 'exit-clean'],
            options: ['stop-when-empty' => true],
            calls: $calls,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($calls[0])->toBe(1)
            ->and($calls[1])->toBe(1);
    });

    it('stop-when-empty propagates a crashed child exit status without restarts', function () {
        $calls = [0 => 0, 1 => 0];
        $supervisor = makeSupervisor(
            workers: 2,
            maxRestarts: 3,
            modes: [0 => 'crash', 1 => 'exit-clean'],
            options: ['stop-when-empty' => true],
            calls: $calls,
        );

        $exit = $supervisor->run();

        // The crash is terminal in once mode: the supervisor exits with the
        // child's exit status instead of entering the restart budget.
        expect($exit)->toBe(1)
            ->and($calls[0])->toBe(1)
            ->and($calls[1])->toBe(1);
    });

    it('stop-when-empty runs inline without pcntl and returns the child exit code', function () {
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;

        $calls = 0;
        $factory = static function () use (&$calls, $stubPath, $stateDir): Process {
            $calls++;

            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => '0',
                'RABBIT_RS_STUB_MODE' => 'crash',
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        // Simulate a PHP build without ext-pcntl (the class exposes the hook for tests).
        $supervisor = new class(plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]], workers: 1, maxRestarts: 1, baseBackoffSeconds: 0, processFactory: $factory, options: ['stop-when-empty' => true]) extends WorkerSupervisor
        {
            protected function canFork(): bool
            {
                return false;
            }
        };

        $exit = $supervisor->run();

        expect($exit)->toBe(1)
            ->and($calls)->toBe(1);
    });

    it('once mode removes exited slots and exits clean without respawn', function () {
        $calls = [];
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'exit-clean'],
            once: true,
            calls: $calls,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($calls)->toBe([0 => 1]);
    });

    it('once mode caps re-arms when children crash without consuming, instead of looping forever', function () {
        $calls = [];
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash'],
            once: true,
            maxWorkers: 3,
            depth: 100,
            calls: $calls,
        );

        $exit = $supervisor->run();

        // Crashed children consumed nothing, so they never renew the re-arm
        // budget: 1 initial child + 2 admitted on the first scale pass +
        // 3 bounded re-arms, then the supervisor gives up and propagates the
        // crash exit status instead of spinning against a broken fleet.
        ksort($calls);
        expect($exit)->toBeGreaterThan(WorkerSupervisor::EXIT_CLEAN)
            ->and(array_keys($calls))->toBe([0, 1, 2, 3, 4, 5])
            ->and($calls)->each->toBe(1);
    });

    it('logs a loud error carrying the child stderr when a once-mode worker exits non-clean', function () {
        // Issue #310: a misconfigured child used to die silently (supervisor
        // never surfaced stderr), so boot failures were invisible to operators.
        Log::shouldReceive('error')->atLeast()->once()->withArgs(
            fn (string $message, array $context): bool => str_contains($message, 'worker exited with status 1')
                && str_contains((string) ($context['stderr'] ?? ''), 'RABBIT_RS_STUB_LOUD'),
        );

        $calls = [];
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash-loud'],
            once: true,
            calls: $calls,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(1)
            ->and($calls)->toBe([0 => 1]);
    });

    it('once mode drains a queue far deeper than the fleet by renewing the re-arm budget on progress', function () {
        // Each child processes one job and exits clean, so the pending depth
        // decreases by exactly one per spawn — the observable progress that
        // must renew the re-arm budget past its absolute cap (issue #269).
        $jobs = 10;
        $spawned = 0;
        $indexes = [];
        $factory = static function (int $workerIndex) use (&$spawned, &$indexes): Process {
            $spawned++;
            $indexes[] = $workerIndex;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: $factory,
            minWorkers: 1,
            maxWorkers: 3,
            once: true,
            depthCallback: static function () use (&$spawned, $jobs): array {
                return ['rabbit-rs' => max(0, $jobs - $spawned)];
            },
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($spawned)->toBe($jobs)
            ->and($indexes)->toBe(range(0, $jobs - 1));
    });

    it('once mode re-probes the depth fresh before its final drain check so a stale cached zero does not strand work', function () {
        // 10 jobs, one per child. The memoized window keeps answering a stale
        // 0 (its cached snapshot), the fresh exit re-check sees the real
        // remaining depth and re-arms (issue #287).
        $jobs = 10;
        $spawned = 0;
        $factory = static function (int $workerIndex) use (&$spawned): Process {
            $spawned++;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: $factory,
            minWorkers: 1,
            maxWorkers: 3,
            once: true,
            depthCallback: static function (bool $fresh = false) use (&$spawned, $jobs): array {
                return ['rabbit-rs' => $fresh ? max(0, $jobs - $spawned) : 0];
            },
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($spawned)->toBe($jobs);   // the stale 0 alone would have exited after the first spawn
    });

    it('once mode re-arms when the broker requeues an in-flight window after the drain check saw zero', function () {
        // Issue #308: --stop-when-empty observed a ready gauge of 0 while the
        // in-flight window was still unacked; the broker requeues it seconds
        // after the consumers leave, and the supervisor used to conclude
        // drained on the first fresh zero — stranding the window.
        $jobs = 10;     // 6 consumed live, 4 requeued by the broker with lag
        $visible = 6;
        $spawned = 0;
        $freshZeroProbes = 0;
        $factory = static function (int $workerIndex) use (&$spawned): Process {
            $spawned++;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: $factory,
            minWorkers: 1,
            maxWorkers: 1,
            once: true,
            depthCallback: static function (bool $fresh = false) use (&$spawned, &$freshZeroProbes, $jobs, $visible): array {
                $ready = max(0, $visible - $spawned);
                if ($fresh && $ready === 0) {
                    $freshZeroProbes++;
                    // The requeued window only shows up on the third fresh
                    // look: gauges lag behind the consumers' departure.
                    if ($freshZeroProbes <= 2) {
                        return ['rabbit-rs' => 0];
                    }

                    return ['rabbit-rs' => max(0, $jobs - $spawned)];
                }

                return ['rabbit-rs' => $ready];
            },
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($spawned)->toBe($jobs);
    });

    it('once mode retries a fully failed fresh probe within the re-arm budget instead of reporting a drained plan', function () {
        $probes = 0;
        $sampler = new QueueDepthSampler(
            [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            static function () use (&$probes): ?int {
                $probes++;

                return null;   // every probe fails
            },
            0.0,
        );
        $spawned = 0;
        $factory = static function (int $workerIndex) use (&$spawned): Process {
            $spawned++;

            return new Process([PHP_BINARY, '-r', 'exit(0);']);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: $factory,
            minWorkers: 1,
            maxWorkers: 3,
            once: true,
            depthCallback: static function (bool $fresh = false) use ($sampler): array {
                return $sampler->depths($fresh);
            },
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)   // bounded: retries, then a clean stop
            ->and($probes)->toBeLessThanOrEqual(20);         // no unbounded probe loop
    });

    it('once mode without a depth source never scales or re-arms', function () {
        $calls = [];
        $supervisor = makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'exit-clean'],
            once: true,
            maxWorkers: 3,
            calls: $calls,
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($calls)->toBe([0 => 1]);
    });

    it('admission scaling grows the fleet to max workers and never beyond', function () {
        $script = writeSupervisorScript(mode: 'run', minWorkers: 1, maxWorkers: 3, depth: 100);
        $process = new Process([PHP_BINARY, $script, test()->stateDir]);
        $process->start();

        // Depth 100 against 1 live worker: the fleet grows to the bound.
        foreach ([0, 1, 2] as $worker) {
            expect(supervisorWaitForMarker($worker, timeoutMs: 4000))->not->toBeNull("worker {$worker} should have been admitted");
        }

        // No worker beyond the bound: the policy never exceeds max-workers,
        // and the cooldown gates the passes while the fleet stays saturated.
        $deadline = microtime(true) + 1.5;
        $overflow = false;
        while (microtime(true) < $deadline) {
            if (supervisorWaitForMarker(3, timeoutMs: 100) !== null) {
                $overflow = true;

                break;
            }
        }

        expect($overflow)->toBeFalse('no worker should be admitted beyond max-workers');

        $supervisorPid = $process->getPid();
        expect($supervisorPid)->not->toBeNull();
        posix_kill($supervisorPid, SIGTERM);

        $process->wait();

        expect($process->getExitCode())->toBe(WorkerSupervisor::EXIT_CLEAN);
    });

    it('does not re-probe the native depth fallback on every scale pass within one ttl window', function () {
        // Native-only connection (no management_url): every scale pass would
        // pay a blocking AMQP round-trip through the native probe (issue #272).
        // The sampler memoizes the probe for its TTL, so the whole supervised
        // run — several scale passes at cooldown 0 — probes exactly once.
        $probes = [];
        $passes = 0;
        $sampler = new QueueDepthSampler(
            [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            static function (string $connection, string $queue) use (&$probes): ?int {
                $probes[] = [$connection, $queue];

                return 5;
            },
            60.0,
        );
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__).WORKER_STUB_PATH;
        $factory = static function (int $workerIndex) use ($stubPath, $stateDir): Process {
            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER_INDEX' => (string) $workerIndex,
                'RABBIT_RS_STUB_MODE' => 'crash',
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        $supervisor = new WorkerSupervisor(
            plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            processFactory: $factory,
            maxWorkers: 2,
            scaleCooldownSeconds: 0.0,
            depthCallback: static function () use ($sampler, &$passes): array {
                $passes++;

                return $sampler->depths();
            },
        );

        $exit = $supervisor->run();

        expect($exit)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS)
            ->and($passes)->toBeGreaterThanOrEqual(2)
            ->and($probes)->toBe([['rabbit-rs', 'default']]);
    });

    it('idle fleet is downscaled to min workers without polluting the restart bookkeeping', function () {
        // Two workers (the ceiling) with a permanently empty queue: after
        // the idle window the highest-index worker is released, the floor
        // worker stays, and the released slot is never recycled.
        $script = writeSupervisorScript(
            mode: 'run',
            minWorkers: 1,
            workers: 2,
            maxWorkers: 2,
            depth: 0,
            scaleIdle: 1,
            scaleCooldown: 1,
        );
        $process = new Process([PHP_BINARY, $script, test()->stateDir]);
        $process->start();

        foreach ([0, 1] as $worker) {
            expect(supervisorWaitForMarker($worker, timeoutMs: 4000))->not->toBeNull("worker {$worker} should have started");
        }

        // The idle window elapses and worker 1 (highest index) is released.
        $exitFile = test()->stateDir.'/worker-1-exited.txt';
        $deadline = microtime(true) + 5.0;
        while (microtime(true) < $deadline) {
            if (is_file($exitFile)) {
                break;
            }
            usleep(20_000);
        }
        expect(is_file($exitFile))->toBeTrue('worker 1 should have been released by the downscale');

        // The floor worker is never signaled.
        expect(is_file(test()->stateDir.'/worker-0-exited.txt'))->toBeFalse();

        // The released slot is removed, not recycled: exactly one invocation.
        usleep(500_000);
        expect(supervisorInvocationCount(1))->toBe(1);

        $supervisorPid = $process->getPid();
        expect($supervisorPid)->not->toBeNull();
        posix_kill($supervisorPid, SIGTERM);

        supervisorAwaitExit($process, 10);

        expect($process->getExitCode())->toBe(WorkerSupervisor::EXIT_CLEAN);
    });

    it('downscale escalates to SIGKILL when a child does not honor SIGTERM', function () {
        $script = writeSupervisorScript(
            mode: 'run',
            minWorkers: 1,
            workers: 2,
            maxWorkers: 2,
            depth: 0,
            scaleIdle: 1,
            scaleCooldown: 1,
            modes: [1 => 'slow-term'],
        );
        $process = new Process([PHP_BINARY, $script, test()->stateDir]);
        $process->start();

        foreach ([0, 1] as $worker) {
            expect(supervisorWaitForMarker($worker, timeoutMs: 4000))->not->toBeNull("worker {$worker} should have started");
        }

        // The downscale SIGTERMs worker 1; the stub records the signal but
        // keeps running (parked graceful shutdown).
        $sigtermFile = test()->stateDir.'/worker-1-sigterm.txt';
        $deadline = microtime(true) + 5.0;
        while (microtime(true) < $deadline) {
            if (is_file($sigtermFile)) {
                break;
            }
            usleep(20_000);
        }
        expect(is_file($sigtermFile))->toBeTrue('worker 1 should have received the downscale SIGTERM');
        $sigtermObservedAt = microtime(true);

        // Wait past the 15 s SIGTERM grace period (STOP_ESCALATION_SECONDS)
        // plus the poll tick: the supervisor must have escalated to SIGKILL
        // by now. A graceful exit would have written the exit marker; SIGKILL
        // cannot.
        $exitFile = test()->stateDir.'/worker-1-exited.txt';
        $deadline = $sigtermObservedAt + 16.5;
        while (microtime(true) < $deadline) {
            usleep(100_000);
        }
        expect(is_file($exitFile))->toBeFalse('worker 1 should have died on SIGKILL, not exited gracefully');

        // The proof of escalation: with worker 1 already gone, the final
        // SIGTERM shuts the supervisor down promptly. Without escalation the
        // parked child would hold the shutdown for Symfony's 10 s fallback.
        $supervisorPid = $process->getPid();
        expect($supervisorPid)->not->toBeNull();
        posix_kill($supervisorPid, SIGTERM);

        supervisorAwaitExit($process, 5);

        expect($process->getExitCode())->toBe(WorkerSupervisor::EXIT_CLEAN);
    });
});

describe('fan-out crash storms (issue #317)', function () {
    /**
     * Deterministic spawn script for a two-entry plan [broken, healthy]:
     * spawn 1 is the broken connection's child (exits 1, a compile failure),
     * spawn 2 is the healthy connection's child (exits 0, drains an empty
     * queue); every later spawn is a broken re-arm or scaler admission (the
     * healthy connection never re-arms or scales). Returns a factory that
     * counts spawns and throws past the cap so an unbounded storm fails the
     * test on a deadline instead of hanging the suite.
     */
    function stormFactory(int $spawnCap, int &$calls): Closure
    {
        return static function () use ($spawnCap, &$calls): Process {
            $calls++;
            if ($calls > $spawnCap) {
                throw new RuntimeException("spawn storm: {$calls} children started, the supervisor never concluded (issue #317)");
            }

            return new Process([PHP_BINARY, '-r', $calls === 2 ? 'exit(0);' : 'exit(1);']);
        };
    }

    it('bounds the once-mode re-arm budget per connection when a fan-out plan mixes a crashing and a draining connection', function () {
        // One broken connection (compile failure: every child exits 1
        // instantly) + one healthy connection draining an empty queue (every
        // child exits 0 instantly, unreadable depth). The once-mode re-arm
        // budget used to be GLOBAL: the healthy connection's clean exits
        // renewed it forever, so the broken connection re-armed endlessly
        // (~12 children/s, worker_index 484 in 40 s on the v0.3.7 roast).
        $calls = 0;
        $supervisor = new WorkerSupervisor(
            plan: [
                ['connection' => 'broken', 'queues' => ['default']],
                ['connection' => 'healthy', 'queues' => ['default']],
            ],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: stormFactory(20, $calls),
            options: ['stop-when-empty' => true],
            depthCallback: static fn (bool $fresh = false): array => ['broken' => 10],
        );

        $exit = $supervisor->run();

        // Bounded: the broken connection burns its own re-arm budget
        // (initial + 3 re-arms) and the healthy one spawns exactly once —
        // 5 children total, then the supervisor propagates the crash exit.
        expect($exit)->toBe(1)
            ->and($calls)->toBe(5);
    });

    it('bounds the fleet when auto-scaling fans out over a crashing connection', function () {
        $calls = 0;
        $supervisor = new WorkerSupervisor(
            plan: [
                ['connection' => 'broken', 'queues' => ['default']],
                ['connection' => 'healthy', 'queues' => ['default']],
            ],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            processFactory: stormFactory(20, $calls),
            options: ['stop-when-empty' => true],
            minWorkers: 1,
            maxWorkers: 3,
            depthCallback: static fn (bool $fresh = false): array => ['broken' => 10],
        );

        $exit = $supervisor->run();

        // Bounded with scaling on: 2 initial + 2 admitted on the first scale
        // pass + 3 bounded re-arms (the healthy connection never scales: its
        // depth is unreadable) = 7 children, then the crash exit propagates.
        expect($exit)->toBe(1)
            ->and($calls)->toBe(7);
    });

    it('stops admitting scaled children once a crashing connection exhausts its re-arm budget', function () {
        // The broken connection settles within its re-arm budget; the
        // healthy connection then spends the (shortened) drain-convergence
        // window idle. Scale passes during that window must not refill the
        // closed broken connection (issue #317): without the closed-entry
        // guard the fleet grows +2 per pass for the whole window.
        $calls = 0;
        $supervisor = new class(plan: [['connection' => 'broken', 'queues' => ['default']], ['connection' => 'healthy', 'queues' => ['default']]], workers: 1, maxRestarts: 3, baseBackoffSeconds: 0, processFactory: stormFactory(PHP_INT_MAX, $calls), options: ['stop-when-empty' => true], minWorkers: 1, maxWorkers: 3, scaleCooldownSeconds: 2.0, depthCallback: static fn (bool $fresh = false): array => ['broken' => 10, 'healthy' => 0]) extends WorkerSupervisor
        {
            protected function drainConvergenceSeconds(): float
            {
                return 3.0;
            }
        };

        $exit = $supervisor->run();

        // Guarded: at most 2 initial + 2 admitted before the broken
        // connection settles + 3 re-arms = 7; the healthy connection drains
        // once and converges through its window.
        expect($exit)->toBe(1)
            ->and($calls)->toBeGreaterThanOrEqual(5)
            ->and($calls)->toBeLessThanOrEqual(8);
    });
});

describe('ready-gauge admission (issue #318)', function () {
    it('admits scaled children on the ready gauge only, never on the fleet in-flight window', function () {
        // One worker claims all 3 jobs instantly (prefetch covers them): the
        // broker reports ready=0, unacked=3 — the summed drain gauge reads 3
        // while the child works. The scaler used to read that summed gauge
        // and admit to max-workers for work that was already claimed. The
        // drain check must still see the full ready+unacked reading (issue
        // #308) — so the scaler's reads must be explicitly flagged
        // ready-only.
        $stateDir = sys_get_temp_dir().'/rabbit-rs-ready-'.uniqid('', true);
        @mkdir($stateDir, 0o777, true);
        $readyOnlyFlagsFile = $stateDir.'/ready-only-flags.txt';
        $spawned = 0;
        $doneSeq = 0;
        $factory = static function () use (&$spawned, &$doneSeq, $stateDir): Process {
            $spawned++;
            $doneFile = $stateDir.'/done-'.(++$doneSeq);

            // The child claims its jobs at boot, works ~200ms, exits clean.
            return new Process([PHP_BINARY, '-r', sprintf('usleep(200000); touch(%s); exit(0);', var_export($doneFile, true))]);
        };

        $supervisor = new class(plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]], workers: 1, maxRestarts: 3, baseBackoffSeconds: 0, processFactory: $factory, minWorkers: 1, maxWorkers: 3, once: true, depthCallback: static function (bool $fresh = false, bool $readyOnly = false) use ($readyOnlyFlagsFile): array {
            file_put_contents($readyOnlyFlagsFile, ($readyOnly ? '1' : '0'), FILE_APPEND);

            if ($readyOnly) {
                // The scaler's view: ready 0 — the fleet claimed everything.
                return ['rabbit-rs' => 0];
            }

            // The drain check's view: ready 0 + unacked 3 until the
            // claiming child completes its window, then fully drained.
            $claimed = glob(dirname($readyOnlyFlagsFile).'/done-*') !== false
                && glob(dirname($readyOnlyFlagsFile).'/done-*') !== [];

            return ['rabbit-rs' => $claimed ? 0 : 3];
        },
        ) extends WorkerSupervisor {
            protected function drainConvergenceSeconds(): float
            {
                return 0.5;
            }
        };

        $exit = $supervisor->run();

        // The scaler never admitted (ready 0): one child consumed its jobs,
        // the fleet stayed at 1, the drain check concluded clean — while
        // every drain read saw the in-flight 3.
        $readyOnlyFlags = file_get_contents($readyOnlyFlagsFile) ?: '';
        supervisorCleanupStateDir($stateDir);

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($spawned)->toBe(1)
            ->and($readyOnlyFlags)->toContain('1');
    });
});

describe('empty-queue drain cost (issue #319)', function () {
    it('concludes an empty-since-boot drain immediately instead of paying the convergence window', function () {
        // The fleet boots on an empty queue: the children exit clean
        // without consuming anything (an empty queue's clean exit is not
        // evidence of consumed work), and the gauge reads 0 from the very
        // first probe — fresh included. The #308 convergence window guards
        // against in-flight work the gauge could still reveal, but when
        // the gauge never read positive during this supervisor's life,
        // there is no such work to catch: conclude drained immediately
        // instead of burning the full window (issue #319).
        $this->stateDir = sys_get_temp_dir().'/rabbit-rs-supervisor-'.uniqid('', true);
        @mkdir($this->stateDir, 0o777, true);

        $startedAt = microtime(true);
        $calls = [];
        $supervisor = makeSupervisor(
            workers: 2,
            maxRestarts: 0,
            options: ['stop-when-empty' => true],
            modes: [0 => 'exit-clean', 1 => 'exit-clean'],
            calls: $calls,
            once: true,
            depth: 0,
        );

        $exit = $supervisor->run();
        $elapsed = microtime(true) - $startedAt;
        supervisorCleanupStateDir($this->stateDir);

        expect($exit)->toBe(WorkerSupervisor::EXIT_CLEAN)
            ->and($calls)->toBe([0 => 1, 1 => 1])
            ->and($elapsed)->toBeLessThan(3.0);
    });
});

/**
 * Bounded wait for a subprocess: a supervisor that never exits fails the
 * test on the deadline instead of hanging the suite.
 */
function supervisorAwaitExit(Process $process, float $timeoutSeconds): void
{
    $deadline = microtime(true) + $timeoutSeconds;
    while ($process->isRunning() && microtime(true) < $deadline) {
        usleep(20_000);
    }
}

/**
 * Build a supervisor that spawns the worker stub instead of queue:work.
 *
 * @param  array<string, string>  $extraEnv  Additional env vars for the child.
 * @param  array<int, string>  $modes  Per-worker stub modes, overriding extraEnv.
 * @param  array<int, int>|null  $calls  Receives the spawn count per worker index.
 * @param  int|null  $depth  When set, injects a depth callback reporting this
 *                           depth for the plan's connection.
 */
function makeSupervisor(
    int $workers,
    int $maxRestarts,
    int $baseBackoffSeconds = 0,
    array $extraEnv = [],
    array $options = [],
    array $modes = [],
    ?array &$calls = null,
    bool $once = false,
    ?int $minWorkers = null,
    ?int $maxWorkers = null,
    ?int $depth = null,
): WorkerSupervisor {
    $stateDir = test()->stateDir;
    $stubPath = dirname(__DIR__).WORKER_STUB_PATH;
    $env = array_merge([
        'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
    ], $extraEnv);

    $factory = static function (int $workerIndex) use ($stubPath, $env, $modes, &$calls): Process {
        if ($calls !== null) {
            $calls[$workerIndex] = ($calls[$workerIndex] ?? 0) + 1;
        }

        $envForChild = array_merge($env, ['RABBIT_RS_WORKER_INDEX' => (string) $workerIndex]);
        if (array_key_exists($workerIndex, $modes)) {
            $envForChild['RABBIT_RS_STUB_MODE'] = $modes[$workerIndex];
        }

        return new Process([PHP_BINARY, $stubPath], null, $envForChild);
    };

    return new WorkerSupervisor(
        plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]],
        workers: $workers,
        maxRestarts: $maxRestarts,
        baseBackoffSeconds: $baseBackoffSeconds,
        processFactory: $factory,
        options: $options,
        minWorkers: $minWorkers,
        maxWorkers: $maxWorkers,
        once: $once,
        depthCallback: $depth !== null ? static fn (): array => ['rabbit-rs' => $depth] : null,
    );
}

function supervisorWaitForMarker(int $worker, int $timeoutMs = 5000): ?array
{
    $marker = test()->stateDir.'/worker-'.$worker.'-started.txt';
    $deadline = microtime(true) + ($timeoutMs / 1000);
    while (microtime(true) < $deadline) {
        if (is_file($marker)) {
            $content = file_get_contents($marker);
            if ($content !== false) {
                $data = json_decode($content, true);

                return is_array($data) ? $data : null;
            }
        }
        usleep(20_000);
    }

    return null;
}

function supervisorInvocationCount(int $worker): int
{
    $file = test()->stateDir.'/worker-'.$worker.'-count.txt';
    if (! is_file($file)) {
        return 0;
    }
    $content = file_get_contents($file);

    return $content === false || $content === '' ? 0 : (int) $content;
}

function supervisorCleanupStateDir(string $dir): void
{
    if (! is_dir($dir)) {
        return;
    }
    $items = glob($dir.'/*');
    if (is_array($items)) {
        foreach ($items as $item) {
            if (is_file($item)) {
                @unlink($item);
            }
        }
    }
    @rmdir($dir);
}

/**
 * Build a self-contained supervisor script for subprocess runs.
 *
 * @param  string  $mode  Stub mode for the child worker.
 * @param  int  $maxRestarts  Supervisor max-restarts budget.
 * @param  int  $baseBackoffSeconds  Supervisor base backoff.
 * @param  int  $workers  Children spawned per plan entry (the initial fleet
 *                        when auto-scaling is configured).
 * @param  array<int, string>  $modes  Per-worker stub modes, overriding $mode.
 * @param  int|null  $minWorkers  Auto-scaling floor (omitted from the
 *                                constructor when null).
 * @param  int|null  $maxWorkers  Auto-scaling ceiling (omitted from the
 *                                constructor when null).
 * @param  int|null  $depth  Depth reported by the injected callback for the
 *                           plan's connection (no callback when null).
 * @param  int|null  $scaleIdle  Scale-down hysteresis window in seconds.
 * @param  int|null  $scaleCooldown  Minimum seconds between scaling passes.
 */
function writeSupervisorScript(
    string $mode = 'run',
    int $maxRestarts = 1,
    int $baseBackoffSeconds = 0,
    int $workers = 1,
    ?int $minWorkers = null,
    ?int $maxWorkers = null,
    ?int $depth = null,
    ?int $scaleIdle = null,
    ?int $scaleCooldown = null,
    array $modes = [],
): string {
    $stubPath = dirname(__DIR__).WORKER_STUB_PATH;
    $autoloadPath = dirname(__DIR__, 2).'/vendor/autoload.php';

    $scalingArgs = '';
    if ($minWorkers !== null) {
        $scalingArgs .= ", minWorkers: {$minWorkers}";
    }
    if ($maxWorkers !== null) {
        $scalingArgs .= ", maxWorkers: {$maxWorkers}";
    }
    if ($scaleIdle !== null) {
        $scalingArgs .= ", scaleIdleSeconds: {$scaleIdle}";
    }
    if ($scaleCooldown !== null) {
        $scalingArgs .= ", scaleCooldownSeconds: {$scaleCooldown}";
    }
    if ($depth !== null) {
        $scalingArgs .= ", depthCallback: static fn (): array => ['rabbit-rs' => {$depth}]";
    }

    // Build a self-contained script that constructs the supervisor and runs it.
    $code = "<?php\n";
    $code .= "declare(strict_types=1);\n";
    $code .= 'require '.var_export($autoloadPath, true).";\n";
    $code .= '$stubPath = '.var_export($stubPath, true).";\n";
    $code .= "\$stateDir = \$argv[1];\n";
    $code .= '$modes = '.var_export($modes, true).";\n";
    $code .= "\$factory = static function (int \$workerIndex) use (\$stubPath, \$stateDir, \$modes): \\Symfony\\Component\\Process\\Process {\n";
    $code .= '    $mode = $modes[$workerIndex] ?? '.var_export($mode, true).";\n";
    $code .= "    \$env = ['RABBIT_RS_WORKER_INDEX'   => (string) \$workerIndex, 'RABBIT_RS_STUB_MODE' => \$mode, 'RABBIT_RS_STUB_STATE_DIR' => \$stateDir];\n";
    $code .= "    return new \\Symfony\\Component\\Process\\Process([PHP_BINARY, \$stubPath], null, \$env);\n";
    $code .= "};\n";
    $code .= "\$supervisor = new \\Goopil\\RabbitRs\\Laravel\\Console\\WorkerSupervisor(\n";
    $code .= "    plan: [['connection' => 'rabbit-rs', 'queues' => ['default']]], workers: {$workers}, maxRestarts: {$maxRestarts}, baseBackoffSeconds: {$baseBackoffSeconds},\n";
    $code .= "    processFactory: \$factory{$scalingArgs},\n";
    $code .= ");\n";
    $code .= "exit(\$supervisor->run());\n";

    $scriptFile = test()->stateDir.'/run-supervisor.php';
    file_put_contents($scriptFile, $code);

    return $scriptFile;
}
