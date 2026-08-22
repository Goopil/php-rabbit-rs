<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class FireAndForgetBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::FIRE_AND_FORGET);
    }

    public function getName(): string { return $this->driver->getName() . ' (fire-and-forget)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
    public function purgeQueue(): void { $this->driver->purgeQueue(); }
    public function runBenchmark(): array { return $this->driver->runBenchmark(); }
}
