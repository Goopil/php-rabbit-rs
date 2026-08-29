<?php

declare(strict_types=1);

function validConfigWithWorkers(): array
{
    return [
        'brokers' => [[
            'name' => 'default',
            'hosts' => [['host' => '127.0.0.1', 'port' => 5672]],
            'vhost' => '/',
            'credentials' => ['username' => 'guest', 'password' => 'native-password-must-stay-secret'],
            'tls' => ['enabled' => false, 'server_name' => null],
            'heartbeat' => 30,
        ]],
        'workers' => [[
            'name' => 'main',
            'subscriptions' => [[
                'name' => 'default',
                'broker' => 'default',
                'queue' => 'jobs',
                'weight' => 1,
                'priority_class' => 0,
                'prefetch' => 16,
            ]],
            'scheduler' => [
                'strategy' => 'weighted_fair',
                'max_in_flight' => 64,
            ],
        ]],
        'topology_mode' => 'external',
    ];
}

describe('config validation', function () {
    it('reports pool stats without exposing credentials', function () {
        $pool = new \Goopil\RabbitRs\Pool(validConfigWithWorkers());
        $stats = $pool->stats();

        expect($stats['closed'])->toBeFalse();
        expect($stats['pid'])->toBe(getmypid());
        expect($stats)->not->toHaveKey('key');
        expect(json_encode($stats))->not->toContain('native-password-must-stay-secret');

        $pool->close();
    });

    it('rejects zero prefetch with the exact path', function () {
        $invalid = validConfigWithWorkers();
        $invalid['workers'][0]['subscriptions'][0]['prefetch'] = 0;

        expect(fn () => new \Goopil\RabbitRs\Pool($invalid))->toThrow(
            function (\Goopil\RabbitRs\Exception $e): void {
                expect($e->getMessage())->toContain('workers.main.subscriptions.default.prefetch');
                expect($e->getMessage())->not->toContain('native-password-must-stay-secret');
            },
        );
    });

    it('rejects legacy max_in_flight with the canonical path', function () {
        $legacy = validConfigWithWorkers();
        $legacy['workers'][0]['max_in_flight'] = 64;
        unset($legacy['workers'][0]['scheduler']['max_in_flight']);

        expect(fn () => new \Goopil\RabbitRs\Pool($legacy))->toThrow(
            function (\Goopil\RabbitRs\Exception $e): void {
                expect($e->getMessage())->toContain('workers.main.max_in_flight');
                expect($e->getMessage())->toContain('workers.main.scheduler.max_in_flight');
            },
        );
    });

    it('rejects recursive configuration', function () {
        $recursive = [];
        $recursive['self'] = &$recursive;

        expect(fn () => new \Goopil\RabbitRs\Pool($recursive))->toThrow(
            fn (\Goopil\RabbitRs\Exception $e) => expect($e->getMessage())->toContain('recursive'),
        );
    });

    it('rejects resource configuration', function () {
        $resourceConfig = validConfigWithWorkers();
        $resourceConfig['unexpected'] = fopen('php://memory', 'r');

        expect(fn () => new \Goopil\RabbitRs\Pool($resourceConfig))->toThrow(
            fn (\Goopil\RabbitRs\Exception $e) => expect($e->getMessage())->toContain('unexpected'),
        );
    });

    it('supports idempotent close', function () {
        $pool = new \Goopil\RabbitRs\Pool(validConfigWithWorkers());
        $pool->close();
        $pool->close();

        try {
            $pool->stats();
            expect(false)->toBeTrue('operation after close must fail');
        } catch (\Goopil\RabbitRs\Exception $e) {
            expect($e->getMessage())->toContain('closed');
        }
    });
});
