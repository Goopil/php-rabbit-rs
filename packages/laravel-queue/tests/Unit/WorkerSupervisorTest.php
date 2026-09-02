<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\WorkerSupervisor;
use Goopil\RabbitRs\Laravel\Exceptions\SupervisorException;

/** queue:work connection flags the supervisor's child commands carry. */
const CONNECTION_ARG_EU = '--connection=eu';
const CONNECTION_ARG_US = '--connection=us';

/**
 * Single-entry plan shaped like WorkPlanResolver output.
 *
 * @return list<array{connection: string, queues: list<string>}>
 */
function singlePlan(string $connection = 'rabbit-rs', string $queue = 'default'): array
{
    return [['connection' => $connection, 'queues' => [$queue]]];
}

describe('buildChildCommands', function (): void {
    it('builds one command per plan entry with workers=1', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: [
                ['connection' => 'eu', 'queues' => ['orders', 'billing.events']],
                ['connection' => 'us', 'queues' => ['orders']],
            ],
            workers: 1,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
        );

        $commands = $supervisor->buildChildCommands();

        expect($commands)->toHaveCount(2)
            ->and($commands[0])->toBe([
                PHP_BINARY,
                'artisan',
                'queue:work',
                CONNECTION_ARG_EU,
                '--queue=orders,billing.events',
                '--name=worker-0',
            ])
            ->and($commands[1])->toBe([
                PHP_BINARY,
                'artisan',
                'queue:work',
                CONNECTION_ARG_US,
                '--queue=orders',
                '--name=worker-1',
            ]);
    });

    it('builds --workers children per plan entry with continuous worker indexes', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: [
                ['connection' => 'eu', 'queues' => ['orders']],
                ['connection' => 'us', 'queues' => ['orders']],
            ],
            workers: 2,
            maxRestarts: 3,
            baseBackoffSeconds: 0,
        );

        $commands = $supervisor->buildChildCommands();

        expect($commands)->toHaveCount(4)
            ->and($commands[0])->toContain(CONNECTION_ARG_EU)
            ->and($commands[1])->toContain(CONNECTION_ARG_EU)
            ->and($commands[2])->toContain(CONNECTION_ARG_US)
            ->and($commands[3])->toContain(CONNECTION_ARG_US)
            ->and($commands[0])->toContain('--name=worker-0')
            ->and($commands[1])->toContain('--name=worker-1')
            ->and($commands[2])->toContain('--name=worker-2')
            ->and($commands[3])->toContain('--name=worker-3');
    });

    it('does not pass an unknown rabbit-rs-worker option', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        foreach ($supervisor->buildChildCommands()[0] as $arg) {
            expect($arg)->not->toContain('--rabbit-rs-worker');
        }
    });
});

describe('buildChildCommands option propagation', function (): void {
    it('omits worker options when none are provided', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        );

        foreach ($supervisor->buildChildCommands()[0] as $arg) {
            expect($arg)->not->toContain('--timeout')
                ->and($arg)->not->toContain('--tries')
                ->and($arg)->not->toContain('--memory')
                ->and($arg)->not->toContain('--max-jobs')
                ->and($arg)->not->toContain('--max-time');
        }
    });

    it('propagates timeout option to child command', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: ['timeout' => 30],
        );

        expect($supervisor->buildChildCommands()[0])->toContain('--timeout=30');
    });

    it('propagates tries option to child command', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: ['tries' => 5],
        );

        expect($supervisor->buildChildCommands()[0])->toContain('--tries=5');
    });

    it('propagates memory option to child command', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: ['memory' => 256],
        );

        expect($supervisor->buildChildCommands()[0])->toContain('--memory=256');
    });

    it('propagates max-jobs option to child command', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: ['max-jobs' => 100],
        );

        expect($supervisor->buildChildCommands()[0])->toContain('--max-jobs=100');
    });

    it('propagates max-time option to child command', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: ['max-time' => 3600],
        );

        expect($supervisor->buildChildCommands()[0])->toContain('--max-time=3600');
    });

    it('propagates all worker options together', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: [
                'timeout'  => 60,
                'tries'    => 3,
                'memory'   => 128,
                'max-jobs' => 500,
                'max-time' => 1800,
            ],
        );

        $cmd = $supervisor->buildChildCommands()[0];

        expect($cmd)->toContain('--timeout=60')
            ->and($cmd)->toContain('--tries=3')
            ->and($cmd)->toContain('--memory=128')
            ->and($cmd)->toContain('--max-jobs=500')
            ->and($cmd)->toContain('--max-time=1800');
    });

    it('omits null-valued options', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
            options: [
                'timeout'  => 30,
                'tries'    => null,
                'memory'   => 128,
                'max-jobs' => null,
                'max-time' => null,
            ],
        );

        $cmd = $supervisor->buildChildCommands()[0];

        expect($cmd)->toContain('--timeout=30')
            ->and($cmd)->toContain('--memory=128');
        foreach ($cmd as $arg) {
            expect($arg)->not->toContain('--tries')
                ->and($arg)->not->toContain('--max-jobs')
                ->and($arg)->not->toContain('--max-time');
        }
    });
});

describe('workerEnvironment', function (): void {
    it('passes the worker index via the environment variable', function (): void {
        $supervisor = new WorkerSupervisor(
            plan: singlePlan(),
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
            plan: singlePlan(),
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
            plan: singlePlan(),
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

describe('pcntl availability', function (): void {
    it('throws SupervisorException when ext-pcntl is missing and more than one child is configured', function (): void {
        $supervisor = new class(
            plan: singlePlan(),
            workers: 2,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        ) extends WorkerSupervisor {
            protected function canFork(): bool
            {
                return false;
            }
        };

        try {
            $supervisor->run();
            expect(false)->toBeTrue('run() should have thrown');
        } catch (SupervisorException $e) {
            expect(str_contains($e->getMessage(), 'ext-pcntl is required'))->toBeTrue();
        }
    });

    it('throws SupervisorException when ext-pcntl is missing and the plan spawns multiple children', function (): void {
        $supervisor = new class(
            plan: [
                ['connection' => 'eu', 'queues' => ['orders']],
                ['connection' => 'us', 'queues' => ['orders']],
            ],
            workers: 1,
            maxRestarts: 1,
            baseBackoffSeconds: 0,
        ) extends WorkerSupervisor {
            protected function canFork(): bool
            {
                return false;
            }
        };

        try {
            $supervisor->run();
            expect(false)->toBeTrue('run() should have thrown');
        } catch (SupervisorException $e) {
            expect(str_contains($e->getMessage(), 'ext-pcntl is required'))->toBeTrue();
        }
    });
});
