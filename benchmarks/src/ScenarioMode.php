<?php

declare(strict_types=1);

namespace Bench;

class ScenarioMode
{
    public const FIRE_AND_FORGET = 'fire-and-forget';
    public const BATCH_CONFIRM = 'batch-confirm';
    public const AUTO_ACK = 'auto-ack';
}
