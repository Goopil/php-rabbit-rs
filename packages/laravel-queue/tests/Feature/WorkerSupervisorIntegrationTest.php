<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Feature;

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Symfony\Component\Process\Process;

/**
 * End-to-end tests for WorkerSupervisor::run().
 *
 * These tests spawn real PHP processes using a minimal stub script instead of
 * `queue:work` so they are deterministic and do not require a Laravel stack or
 * broker connection in the children. The supervisor is constructed with a
 * custom process factory that launches the stub.
 */
final class WorkerSupervisorIntegrationTest extends TestCase
{
    private string $stateDir;

    protected function setUp(): void
    {
        parent::setUp();
        $this->stateDir = sys_get_temp_dir() . '/rabbit-rs-supervisor-' . uniqid('', true);
        @mkdir($this->stateDir, 0o777, true);
    }

    protected function tearDown(): void
    {
        $this->cleanupStateDir($this->stateDir);
        parent::tearDown();
    }

    /**
     * Build a supervisor that spawns the worker stub instead of queue:work.
     *
     * @param array<string, string> $extraEnv Additional env vars for the child.
     */
    private function makeSupervisor(
        int $workers,
        int $maxRestarts,
        int $baseBackoffSeconds = 0,
        array $extraEnv = [],
    ): WorkerSupervisor {
        $stateDir = $this->stateDir;
        $stubPath = dirname(__DIR__) . '/Fixture/worker_stub.php';
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

    private function waitForMarker(int $worker, int $timeoutMs = 5000): ?array
    {
        $marker = $this->stateDir . '/worker-' . $worker . '-started.txt';
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

    private function invocationCount(int $worker): int
    {
        $file = $this->stateDir . '/worker-' . $worker . '-count.txt';
        if (! is_file($file)) {
            return 0;
        }
        $content = file_get_contents($file);

        return $content === false || $content === '' ? 0 : (int) $content;
    }

    private function cleanupStateDir(string $dir): void
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

    public function testSpawnedWorkerReceivesWorkerIndexViaEnvironment(): void
    {
        $supervisor = $this->makeSupervisor(workers: 1, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'exit-clean',
        ]);

        $supervisor->run();

        // The marker file records the worker index received by the child.
        $marker = $this->waitForMarker(0);
        self::assertNotNull($marker, 'Worker 0 should have started and written a marker');
        self::assertSame(0, $marker['worker']);
    }

    public function testMultipleWorkersEachReceiveDistinctIndex(): void
    {
        $supervisor = $this->makeSupervisor(workers: 2, maxRestarts: 1, extraEnv: [
            'RABBIT_RS_STUB_MODE' => 'exit-clean',
        ]);

        $supervisor->run();

        $marker0 = $this->waitForMarker(0);
        $marker1 = $this->waitForMarker(1);
        self::assertNotNull($marker0, 'Worker 0 should have started');
        self::assertNotNull($marker1, 'Worker 1 should have started');
        self::assertSame(0, $marker0['worker']);
        self::assertSame(1, $marker1['worker']);
    }

    public function testCrashedWorkerIsRestartedWithBackoff(): void
    {
        $supervisor = $this->makeSupervisor(
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash'],
        );

        $exit = $supervisor->run();

        // Each crash is a non-zero exit; the supervisor restarts until maxRestarts.
        self::assertSame(WorkerSupervisor::EXIT_MAX_RESTARTS, $exit);
        self::assertGreaterThan(1, $this->invocationCount(0));
    }

    public function testMaxRestartsReachedReturnsExitMaxRestarts(): void
    {
        $supervisor = $this->makeSupervisor(
            workers: 1,
            maxRestarts: 2,
            baseBackoffSeconds: 0,
            extraEnv: ['RABBIT_RS_STUB_MODE' => 'crash'],
        );

        $exit = $supervisor->run();

        self::assertSame(WorkerSupervisor::EXIT_MAX_RESTARTS, $exit);
        // Initial + maxRestarts attempts.
        self::assertSame(1 + 2, $this->invocationCount(0));
    }

    public function testRunModeWorkerThenSignalReturnsCleanExit(): void
    {
        // Run the supervisor in a subprocess so we can send it a signal.
        $script = $this->writeSupervisorScript();
        $process = new Process([PHP_BINARY, $script, $this->stateDir]);
        $process->start();

        // Wait for the worker to start.
        $marker = null;
        $deadline = microtime(true) + 5.0;
        while (microtime(true) < $deadline) {
            $marker = $this->waitForMarker(0, timeoutMs: 100);
            if ($marker !== null) {
                break;
            }
        }
        self::assertNotNull($marker, 'Worker should have started before sending SIGTERM');

        usleep(100_000); // Give the supervisor a moment to enter its loop.

        $supervisorPid = $process->getPid();
        self::assertNotNull($supervisorPid);
        posix_kill($supervisorPid, SIGTERM);

        $process->wait();
        $exitCode = $process->getExitCode();

        self::assertSame(WorkerSupervisor::EXIT_CLEAN, $exitCode);
    }

    private function writeSupervisorScript(): string
    {
        $stubPath = dirname(__DIR__) . '/Fixture/worker_stub.php';
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

        $scriptFile = $this->stateDir . '/run-supervisor.php';
        file_put_contents($scriptFile, $code);

        return $scriptFile;
    }
}
