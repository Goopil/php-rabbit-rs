<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;

describe('buildChildCommand', function (): void {
    it('constructs the child command with a single worker', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
        );

        $command = $supervisor->buildChildCommand();

        expect($command)->toContain('queue:work')
            ->and($command)->toContain('--connection=rabbit-rs')
            ->and($command)->toContain('--queue=default');
    });

    it('constructs the child command with multiple workers', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'orders',
            workers: 3,
            maxRestarts: 5,
            baseBackoffSeconds: 0,
        );

        $command = $supervisor->buildChildCommand();

        expect($command)->toContain('queue:work')
            ->and($command)->toContain('--connection=rabbit-rs')
            ->and($command)->toContain('--queue=orders');
    });

    it('includes the worker index in the name option', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $cmd0 = $supervisor->buildChildCommand(workerIndex: 0);
        $cmd1 = $supervisor->buildChildCommand(workerIndex: 1);

        expect($cmd0)->toContain('--name=worker-0')
            ->and($cmd1)->toContain('--name=worker-1');
    });

    it('does not pass an unknown rabbit-rs-worker option', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $cmd = $supervisor->buildChildCommand();

        foreach ($cmd as $arg) {
            expect($arg)->not->toContain('--rabbit-rs-worker');
        }
    });
});

describe('workerEnvironment', function (): void {
    it('passes the worker index via the environment variable', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        $env0 = $supervisor->workerEnvironment(0);
        $env1 = $supervisor->workerEnvironment(1);

        expect('0')->toBe($env0[WorkerSupervisor::workerEnv()])
            ->and('1')->toBe($env1[WorkerSupervisor::workerEnv()]);
    });
});

describe('shouldRestart', function (): void {
    it('respects the max restarts limit', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 2,
            baseBackoffSeconds: 0,
        );

        expect($supervisor->shouldRestart(0))->toBeTrue()
            ->and($supervisor->shouldRestart(1))->toBeTrue()
            ->and($supervisor->shouldRestart(2))->toBeFalse();
    });
});

describe('exit codes', function (): void {
    it('exposes the exit code for max restarts exceeded', function (): void {
        expect(1)->toBe(WorkerSupervisor::EXIT_MAX_RESTARTS);
    });

    it('exposes the exit code for a clean shutdown', function (): void {
        expect(0)->toBe(WorkerSupervisor::EXIT_CLEAN);
    });

    it('exposes the exit code for a signal received', function (): void {
        expect(130)->toBe(WorkerSupervisor::EXIT_SIGNAL);
    });
});

describe('backoff', function (): void {
    it('increases exponentially and caps at 60 seconds', function (): void {
        $supervisor = new WorkerSupervisor(
            connection: 'rabbit-rs',
            queue: 'default',
            workers: 1,
            maxRestarts: 10,
            baseBackoffSeconds: 1,
        );

        // 2^0 = 1, 2^1 = 2, 2^2 = 4, 2^3 = 8, 2^4 = 16, 2^5 = 32, 2^6 = 64 → capped at 60
        expect(1)->toBe($supervisor->backoffSeconds(0))
            ->and(2)->toBe($supervisor->backoffSeconds(1))
            ->and(4)->toBe($supervisor->backoffSeconds(2))
            ->and(8)->toBe($supervisor->backoffSeconds(3))
            ->and(16)->toBe($supervisor->backoffSeconds(4))
            ->and(32)->toBe($supervisor->backoffSeconds(5))
            ->and(60)->toBe($supervisor->backoffSeconds(6))
            ->and(60)->toBe($supervisor->backoffSeconds(7))
            ->and(60)->toBe($supervisor->backoffSeconds(100));
    });
});
