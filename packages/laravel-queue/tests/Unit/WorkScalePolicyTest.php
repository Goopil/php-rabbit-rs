<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Console\ScaleState;
use Goopil\RabbitRs\Laravel\Console\WorkScalePolicy;

describe('WorkScalePolicy scale up', function (): void {
    it('scales up when depth exceeds twice the live workers', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 10, 1, $state);

        expect($action->up)->toBe(2)
            ->and($action->down)->toBe(0);
    });

    it('does not scale up at exactly the twice-live threshold', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 2, 1, $state);

        expect($action->up)->toBe(0)
            ->and($action->down)->toBe(0);
    });

    it('scales up from zero live workers on any depth', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 1, 0, $state);

        expect($action->up)->toBe(2);
    });

    it('caps the shift at two workers per pass', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 100, 0, $state);

        expect($action->up)->toBe(2);
    });

    it('clamps the shift to the max-workers bound', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 3);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 100, 2, $state);

        expect($action->up)->toBe(1);
    });

    it('never scales up beyond max workers', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 3);
        $state = new ScaleState;

        $action = $policy->decide(100.0, 100, 3, $state);

        expect($action->up)->toBe(0);
    });
});

describe('WorkScalePolicy cooldown', function (): void {
    it('blocks new actions within the cooldown window', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0);
        $state = new ScaleState;

        expect($policy->decide(100.0, 10, 1, $state)->up)->toBe(2)
            ->and($policy->decide(101.0, 10, 3, $state)->up)->toBe(0);
    });

    it('acts again once the cooldown has elapsed', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0);
        $state = new ScaleState;

        expect($policy->decide(100.0, 10, 1, $state)->up)->toBe(2)
            ->and($policy->decide(103.0, 10, 3, $state)->up)->toBe(2);
    });
});

describe('WorkScalePolicy scale down', function (): void {
    it('holds idle workers through the hysteresis window', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 30);
        $state = new ScaleState;

        expect($policy->decide(100.0, 0, 3, $state)->down)->toBe(0)
            ->and($policy->decide(129.0, 0, 3, $state)->down)->toBe(0);
    });

    it('scales down after continuous empty depth', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 30);
        $state = new ScaleState;

        $policy->decide(100.0, 0, 3, $state);

        $action = $policy->decide(130.0, 0, 3, $state);

        expect($action->down)->toBe(2)
            ->and($action->up)->toBe(0);
    });

    it('resets the empty streak when depth returns above zero', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 30);
        $state = new ScaleState;

        $policy->decide(100.0, 0, 3, $state);
        $policy->decide(110.0, 5, 3, $state);
        $policy->decide(120.0, 0, 3, $state);

        expect($policy->decide(149.0, 0, 3, $state)->down)->toBe(0)
            ->and($policy->decide(150.0, 0, 3, $state)->down)->toBe(2);
    });

    it('clamps the downshift above min workers', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 30);
        $state = new ScaleState;

        $policy->decide(100.0, 0, 2, $state);

        expect($policy->decide(130.0, 0, 2, $state)->down)->toBe(1);
    });

    it('never scales down at or below min workers', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 30);
        $state = new ScaleState;

        $policy->decide(100.0, 0, 1, $state);

        expect($policy->decide(130.0, 0, 1, $state)->down)->toBe(0);
    });

    it('tracks the empty streak across cooldown-blocked passes', function (): void {
        $policy = new WorkScalePolicy(minWorkers: 1, maxWorkers: 10, cooldownSeconds: 3.0, idleSeconds: 5);
        $state = new ScaleState;

        $policy->decide(100.0, 0, 3, $state);

        expect($policy->decide(102.0, 0, 3, $state)->down)->toBe(0)
            ->and($policy->decide(105.0, 0, 3, $state)->down)->toBe(2);
    });
});
