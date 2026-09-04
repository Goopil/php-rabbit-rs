<?php

declare(strict_types=1);

use Bench\AbstractBenchmark;

/**
 * Broker-free driver: no-ops the publish/consume phases so runBenchmark()
 * can be exercised for its result contract alone.
 */
final class ContractProbeDriver extends AbstractBenchmark
{
    public function __construct(
        private readonly ?string $safety = null,
        private readonly ?int $reconnects = null,
    ) {
    }

    public function getName(): string
    {
        return 'contract-probe';
    }

    public function setUp(): void {}
    public function tearDown(): void {}
    public function publishMessages(int $count): void { usleep(1); }

    public function consumeMessages(int $count): void
    {
        usleep(1);
        foreach ([1.0, 2.0, 3.0, 4.0, 100.0] as $ms) {
            $this->recordLatency($ms);
        }
        $this->recordReceived('a');
        $this->recordReceived('a'); // duplicate
    }

    public function safetyMode(): ?string
    {
        return $this->safety;
    }

    public function reconnects(): ?int
    {
        return $this->reconnects;
    }
}

class_exists(ContractProbeDriver::class);

it('surfaces latency percentiles, losses and duplicates', function () {
    $stats = (new ContractProbeDriver)->runBenchmark();

    expect($stats['consume']['p50'])->toBe(3.0)
        ->and($stats['consume']['p95'])->toBe(100.0)
        ->and($stats['consume']['p99'])->toBe(100.0)
        ->and($stats['consume']['losses'])->toBeGreaterThanOrEqual(0)
        ->and($stats['consume']['duplicates'])->toBe(10); // 1/round × 10 rounds
});

it('surfaces safety mode, reconnects and stall recoveries', function () {
    $stats = (new ContractProbeDriver(safety: 'safe', reconnects: 2))->runBenchmark();

    expect($stats['safety'])->toBe('safe')
        ->and($stats['reconnects'])->toBe(2)
        ->and($stats['stall_recoveries'])->toBe(0);
});

it('defaults safety and reconnects to null when the driver cannot surface them', function () {
    $stats = (new ContractProbeDriver)->runBenchmark();

    expect($stats['safety'])->toBeNull()
        ->and($stats['reconnects'])->toBeNull()
        ->and($stats['stall_recoveries'])->toBe(0);
});
