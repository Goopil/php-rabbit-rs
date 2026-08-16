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
}
