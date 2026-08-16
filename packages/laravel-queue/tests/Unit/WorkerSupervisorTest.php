<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Unit;

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Symfony\Component\Process\Process;

final class WorkerSupervisorTest extends TestCase
{
    public function testConstructsChildCommandWithSingleWorker(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
        );

        $command = $supervisor->buildChildCommand();

        self::assertContains('queue:work', $command);
        self::assertContains('--connection=rabbit-rs', $command);
        self::assertContains('--queue=default', $command);
    }

    public function testConstructsChildCommandWithMultipleWorkers(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'orders',
            workers: 3,
            maxRestarts: 5,
            baseBackoffSeconds: 0,
        );

        $command = $supervisor->buildChildCommand();

        self::assertContains('queue:work', $command);
        self::assertContains('--connection=rabbit-rs', $command);
        self::assertContains('--queue=orders', $command);
    }

    public function testBuildChildCommandIncludesWorkerIndexInNameOption(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $cmd0 = $supervisor->buildChildCommand(workerIndex: 0);
        $cmd1 = $supervisor->buildChildCommand(workerIndex: 1);

        self::assertContains('--name=worker-0', $cmd0);
        self::assertContains('--name=worker-1', $cmd1);
    }

    public function testWorkerEnvironmentPassesIndexViaEnvVar(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $env0 = $supervisor->workerEnvironment(0);
        $env1 = $supervisor->workerEnvironment(1);

        self::assertSame('0', $env0[WorkerSupervisor::workerEnv()]);
        self::assertSame('1', $env1[WorkerSupervisor::workerEnv()]);
    }

    public function testBuildChildCommandDoesNotPassUnknownRabbitRsWorkerOption(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $cmd = $supervisor->buildChildCommand();

        foreach ($cmd as $arg) {
            self::assertStringNotContainsString('--rabbit-rs-worker', $arg);
        }
    }

    public function testMaxRestartsIsRespected(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 2,
            baseBackoffSeconds: 0,
        );

        $restarts = $supervisor->shouldRestart(0);
        self::assertTrue($restarts);

        $restarts = $supervisor->shouldRestart(1);
        self::assertTrue($restarts);

        $restarts = $supervisor->shouldRestart(2);
        self::assertFalse($restarts);
    }

    public function testExitCodeForMaxRestartsExceeded(): void
    {
        self::assertSame(1, WorkerSupervisor::EXIT_MAX_RESTARTS);
    }

    public function testExitCodeForCleanShutdown(): void
    {
        self::assertSame(0, WorkerSupervisor::EXIT_CLEAN);
    }

    public function testExitCodeForSignalReceived(): void
    {
        self::assertSame(130, WorkerSupervisor::EXIT_SIGNAL);
    }

    public function testBackoffSecondsExponentiallyIncreasesAndCapsAt60(): void
    {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 10,
            baseBackoffSeconds: 1,
        );

        // 2^0 = 1, 2^1 = 2, 2^2 = 4, 2^3 = 8, 2^4 = 16, 2^5 = 32, 2^6 = 64 → capped at 60
        self::assertSame(1, $supervisor->backoffSeconds(0));
        self::assertSame(2, $supervisor->backoffSeconds(1));
        self::assertSame(4, $supervisor->backoffSeconds(2));
        self::assertSame(8, $supervisor->backoffSeconds(3));
        self::assertSame(16, $supervisor->backoffSeconds(4));
        self::assertSame(32, $supervisor->backoffSeconds(5));
        self::assertSame(60, $supervisor->backoffSeconds(6));
        self::assertSame(60, $supervisor->backoffSeconds(7));
        self::assertSame(60, $supervisor->backoffSeconds(100));
    }
}
