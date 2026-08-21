<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;

describe('WorkerProfileResolver', function () {
    it('returns the profile that subscribes to the queue', function () {
        $resolver = new WorkerProfileResolver([
            [
                'name' => 'default',
                'subscriptions' => [
                    ['name' => 'orders', 'queue' => 'orders-eu'],
                    ['name' => 'billing', 'queue' => 'billing-eu'],
                ],
            ],
            [
                'name' => 'high-priority',
                'subscriptions' => [
                    ['name' => 'urgent', 'queue' => 'urgent-eu'],
                ],
            ],
        ]);

        expect($resolver->profileForQueue('orders-eu'))->toBe('default')
            ->and($resolver->profileForQueue('billing-eu'))->toBe('default')
            ->and($resolver->profileForQueue('urgent-eu'))->toBe('high-priority');
    });

    it('returns null when no profile subscribes to the queue', function () {
        $resolver = new WorkerProfileResolver([
            [
                'name' => 'default',
                'subscriptions' => [
                    ['name' => 'orders', 'queue' => 'orders-eu'],
                    ['name' => 'billing', 'queue' => 'billing-eu'],
                ],
            ],
            [
                'name' => 'high-priority',
                'subscriptions' => [
                    ['name' => 'urgent', 'queue' => 'urgent-eu'],
                ],
            ],
        ]);

        expect($resolver->profileForQueue('unknown-queue'))->toBeNull();
    });

    it('returns the first match when multiple profiles subscribe to the same queue', function () {
        $resolver = new WorkerProfileResolver([
            [
                'name' => 'first',
                'subscriptions' => [
                    ['name' => 'main', 'queue' => 'shared-queue'],
                ],
            ],
            [
                'name' => 'second',
                'subscriptions' => [
                    ['name' => 'backup', 'queue' => 'shared-queue'],
                ],
            ],
        ]);

        expect($resolver->profileForQueue('shared-queue'))->toBe('first');
    });
});
