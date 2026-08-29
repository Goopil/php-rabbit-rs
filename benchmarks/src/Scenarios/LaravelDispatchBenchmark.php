<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\Config;
use Bench\ScenarioMode;

class LaravelDispatchBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::LARAVEL_DISPATCH);
        $driver->payloadBytes = Config::MESSAGE_PAYLOAD_LARAVEL_BYTES;
    }

    public function getName(): string { return $this->driver->getName() . ' (laravel-dispatch)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
    public function purgeQueue(): void { $this->driver->purgeQueue(); }
    public function runBenchmark(): array { return $this->driver->runBenchmark(); }
}
