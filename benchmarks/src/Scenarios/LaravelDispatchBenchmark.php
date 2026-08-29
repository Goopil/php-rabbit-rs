<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class LaravelDispatchBenchmark extends AbstractLaravelScenarioBenchmark
{
    public function __construct(AbstractBenchmark $driver)
    {
        parent::__construct($driver, ScenarioMode::LARAVEL_DISPATCH, 'laravel-dispatch');
    }
}
