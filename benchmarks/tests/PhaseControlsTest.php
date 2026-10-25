<?php

use Bench\AbstractBenchmark;

/**
 * Broker-free driver exercising the phase controls the issue-#368 comparison
 * harness uses instead of runBenchmark().
 */
final class PhaseProbeDriver extends AbstractBenchmark
{
    public function getName(): string
    {
        return 'phase-probe';
    }

    public function setUp(): void {}
    public function tearDown(): void {}
    public function publishMessages(int $count): void {}

    public function consumeMessages(int $count): void
    {
        $this->recordLatency(1.0);
        $this->recordReceived('m1');
        $this->recordReceived('m2');
    }
}

class_exists(PhaseProbeDriver::class);

it('gates phase losses against the expected count of the phase', function () {
    $driver = new PhaseProbeDriver;
    $driver->beginPhase();
    $driver->expectCount(2);

    $driver->consumeMessages(2);

    expect($driver->phaseReport())->toBe([
        'expected' => 2,
        'received' => 2,
        'losses' => 0,
        'duplicates' => 0,
        'latency_ms' => ['p50' => 1.0, 'p95' => 1.0, 'p99' => 1.0],
    ]);
});

it('resets tracking and the expected count on beginPhase', function () {
    $driver = new PhaseProbeDriver;
    $driver->beginPhase();
    $driver->expectCount(5);
    $driver->consumeMessages(5);

    $driver->beginPhase();

    expect($driver->phaseReport()['expected'])->toBe(10000) // Config::MESSAGE_COUNT
        ->and($driver->phaseReport()['received'])->toBe(0);
});
