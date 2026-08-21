<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class BatchConfirmBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::BATCH_CONFIRM);
    }

    public function getName(): string { return $this->driver->getName() . ' (batch-confirm)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
}
