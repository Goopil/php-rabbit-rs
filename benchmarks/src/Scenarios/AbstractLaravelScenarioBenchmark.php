<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\Config;

abstract class AbstractLaravelScenarioBenchmark extends AbstractBenchmark
{
    public function __construct(
        private readonly AbstractBenchmark $driver,
        string $mode,
        private readonly string $label,
    ) {
        $driver->setScenarioMode($mode);
        $driver->payloadBytes = Config::MESSAGE_PAYLOAD_LARAVEL_BYTES;
    }

    public function getName(): string { return $this->driver->getName() . ' (' . $this->label . ')'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
    public function purgeQueue(): void { $this->driver->purgeQueue(); }
    public function runBenchmark(): array { return $this->driver->runBenchmark(); }
}
