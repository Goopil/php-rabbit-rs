<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Symfony\Component\Process\Process;

const WORKER_STUB_PATH = '/Fixture/worker_stub.php';

describe('WorkerSupervisor integration', function () {
    beforeEach(function () {
        $this->stateDir = sys_get_temp_dir() . '/rabbit-rs-supervisor-' . uniqid('', true);
        @mkdir($this->stateDir, 0o777, true);
    });

    afterEach(function () {
        supervisorCleanupStateDir($this->stateDir);
    });

    it('spawned worker receives worker index via environment', function () {
        $supervisor = makeSupervisor(workers: 1, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'exit-clean',
        ]);

        $supervisor->run();

        // The marker file records the worker index received by the child.
        $marker = supervisorWaitForMarker(0);
        expect($marker)->not->toBeNull('Worker 0 should have started and written a marker');
        expect($marker['worker'])->toBe(0);
    });

    it('multiple workers each receive distinct index', function () {
        $supervisor = makeSupervisor(workers: 2, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'exit-clean',
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

    it('stops all children when one worker exceeds max restarts', function () {
        // Worker 0 crashes immediately (exhausting max-restarts quickly).
        // Worker 1 runs until signaled ("run" mode).
        // The supervisor must stop worker 1 before returning EXIT_MAX_RESTARTS.
        $stateDir = test()->stateDir;
        $stubPath = dirname(__DIR__) . WORKER_STUB_PATH;

        $factory = static function (int $workerIndex) use ($stubPath, $stateDir): Process {
            $cmd = [PHP_BINARY, $stubPath];
            $mode = $workerIndex === 0 ? 'crash' : 'run';
            $envForChild = [
                'RABBIT_RS_WORKER'           => (string) $workerIndex,
                'RABBIT_RS_STUB_MODE'        => $mode,
                'RABBIT_RS_STUB_STATE_DIR'   => $stateDir,
            ];

            return new Process($cmd, null, $envForChild);
        };

        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
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

        $exitFile = $stateDir . '/worker-1-exited.txt';
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
        $stubPath = dirname(__DIR__) . WORKER_STUB_PATH;

        $factory = static function (int $workerIndex) use ($stubPath, $stateDir): Process {
            return new Process([PHP_BINARY, $stubPath], null, [
                'RABBIT_RS_WORKER'         => (string) $workerIndex,
                'RABBIT_RS_STUB_MODE'      => 'exit-clean',
                'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
            ]);
        };

        // Simulate a PHP build without ext-pcntl (the class exposes the hook for tests).
        $supervisor = new class(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            processFactory: $factory,
        ) extends WorkerSupervisor {
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
        $stubPath = dirname(__DIR__) . WORKER_STUB_PATH;

        $starts = [0 => [], 1 => []];
        $factory = static function (int $workerIndex) use (&$starts, $stubPath, $stateDir): Process {
            $starts[$workerIndex][] = microtime(true);

            if ($workerIndex === 0) {
                // Crashes immediately on every start.
                return new Process([PHP_BINARY, $stubPath], null, [
                    'RABBIT_RS_WORKER'         => '0',
                    'RABBIT_RS_STUB_MODE'      => 'crash',
                    'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
                ]);
            }

            // Stays up for a moment, then exits non-zero: its crash lands well
            // inside worker 0's backoff window.
            return new Process([PHP_BINARY, '-r', 'usleep(500000); exit(1);'], null, [
                'RABBIT_RS_WORKER' => '1',
            ]);
        };

        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
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
});

/**
 * Build a supervisor that spawns the worker stub instead of queue:work.
 *
 * @param array<string, string> $extraEnv Additional env vars for the child.
 */
function makeSupervisor(
    int $workers,
    int $maxRestarts,
    int $baseBackoffSeconds = 0,
    array $extraEnv = [],
): WorkerSupervisor {
    $stateDir = test()->stateDir;
    $stubPath = dirname(__DIR__) . WORKER_STUB_PATH;
    $env = array_merge([
        'RABBIT_RS_STUB_STATE_DIR' => $stateDir,
    ], $extraEnv);

    $factory = static function (int $workerIndex) use ($stubPath, $env): Process {
        $cmd = [
            PHP_BINARY,
            $stubPath,
        ];
        $envForChild = array_merge($env, ['RABBIT_RS_WORKER' => (string) $workerIndex]);

        return new Process($cmd, null, $envForChild);
    };

    return new WorkerSupervisor(
        connection: 'rabbit-rs',
        queue: 'default',
        workers: $workers,
        maxRestarts: $maxRestarts,
        baseBackoffSeconds: $baseBackoffSeconds,
        processFactory: $factory,
    );
}

function supervisorWaitForMarker(int $worker, int $timeoutMs = 5000): ?array
{
    $marker = test()->stateDir . '/worker-' . $worker . '-started.txt';
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
    $file = test()->stateDir . '/worker-' . $worker . '-count.txt';
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
    $items = glob($dir . '/*');
    if (is_array($items)) {
        foreach ($items as $item) {
            if (is_file($item)) {
                @unlink($item);
            }
        }
    }
    @rmdir($dir);
}

function writeSupervisorScript(): string
{
    $stubPath = dirname(__DIR__) . WORKER_STUB_PATH;
    $autoloadPath = dirname(__DIR__, 2) . '/vendor/autoload.php';

    // Build a self-contained script that constructs the supervisor and runs it.
    $code = "<?php\n";
    $code .= "declare(strict_types=1);\n";
    $code .= "require " . var_export($autoloadPath, true) . ";\n";
    $code .= "\$stubPath = " . var_export($stubPath, true) . ";\n";
    $code .= "\$stateDir = \$argv[1];\n";
    $code .= "\$factory = static function (int \$workerIndex) use (\$stubPath, \$stateDir): \\Symfony\\Component\\Process\\Process {\n";
    $code .= "    \$env = ['RABBIT_RS_WORKER' => (string) \$workerIndex, 'RABBIT_RS_STUB_MODE' => 'run', 'RABBIT_RS_STUB_STATE_DIR' => \$stateDir];\n";
    $code .= "    return new \\Symfony\\Component\\Process\\Process([PHP_BINARY, \$stubPath], null, \$env);\n";
    $code .= "};\n";
    $code .= "\$supervisor = new \\Goopil\\RabbitRs\\Laravel\\Console\\WorkerSupervisor(\n";
    $code .= "    connection: 'rabbit-rs', queue: 'default', workers: 1, maxRestarts: 1, baseBackoffSeconds: 0,\n";
    $code .= "    processFactory: \$factory,\n";
    $code .= ");\n";
    $code .= "exit(\$supervisor->run());\n";

    $scriptFile = test()->stateDir . '/run-supervisor.php';
    file_put_contents($scriptFile, $code);

    return $scriptFile;
}
