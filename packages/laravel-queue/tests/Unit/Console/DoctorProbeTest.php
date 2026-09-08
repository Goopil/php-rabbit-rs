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
