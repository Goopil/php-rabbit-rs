<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\DoctorProbe;

describe('DoctorProbe', function () {
    it('passes the compiled config into the broker probe closure', function (): void {
        // Regression: the probe closure referenced $nativeConfig without
        // capturing it, so every broker check failed with a TypeError that
        // the probe pool converted into a bogus "broker unreachable" report
        // on healthy setups (issue: doctor fails its core checks).
        if (extension_loaded('rabbit_rs')) {
            $this->markTestSkipped('the real Pool bypasses the bootstrap fake');
        }

        $probe = new DoctorProbe;

        $config = [
            'brokers' => [
                ['name' => 'eu', 'hosts' => [['host' => 'eu.rabbit.local', 'port' => 5672]], 'vhost' => '/'],
            ],
            'workers' => [
                ['name' => 'main', 'subscriptions' => [
                    ['name' => 'jobs', 'broker' => 'eu', 'queue' => 'jobs'],
                ]],
            ],
        ];

        expect($probe->broker($config))->toBeNull();
    });
});

describe('DoctorProbe::declareConfig', function () {
    it('bounds the consumer wait timeout while preserving every other key', function (): void {
        $config = [
            'brokers' => [
                ['name' => 'eu', 'hosts' => [['host' => 'eu.rabbit.local', 'port' => 5672]], 'vhost' => '/'],
            ],
            'topology_mode' => 'declare',
            'workers' => [
                ['name' => 'main', 'subscriptions' => [
                    ['name' => 'jobs', 'broker' => 'eu', 'queue' => 'jobs'],
                ]],
            ],
            'consumer' => ['prefetch' => 16, 'wait_timeout' => 30_000, 'max_attempts' => 3],
        ];

        $probed = DoctorProbe::declareConfig($config);

        expect($probed['consumer']['wait_timeout'])->toBe(2000)
            ->and($probed['consumer']['prefetch'])->toBe(16)
            ->and($probed['consumer']['max_attempts'])->toBe(3)
            ->and($probed['topology_mode'])->toBe('declare')
            ->and($probed['brokers'])->toBe($config['brokers'])
            ->and($probed['workers'])->toBe($config['workers']);
    });

    it('does not mutate the input config', function (): void {
        $config = [
            'brokers' => [
                ['name' => 'eu', 'hosts' => [['host' => 'eu.rabbit.local', 'port' => 5672]], 'vhost' => '/'],
            ],
            'consumer' => ['prefetch' => 16, 'wait_timeout' => 30_000],
        ];

        DoctorProbe::declareConfig($config);

        expect($config['consumer']['wait_timeout'])->toBe(30_000)
            ->and($config['consumer']['prefetch'])->toBe(16);
    });

    it('creates the consumer section when missing', function (): void {
        $probed = DoctorProbe::declareConfig(['topology_mode' => 'declare']);

        expect($probed['consumer'])->toBe(['wait_timeout' => 2000]);
    });

    it('replaces a non-array consumer section', function (): void {
        $probed = DoctorProbe::declareConfig(['consumer' => 'invalid']);

        expect($probed['consumer'])->toBe(['wait_timeout' => 2000]);
    });
});
