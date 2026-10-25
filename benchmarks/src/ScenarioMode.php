<?php

declare(strict_types=1);

namespace Bench;

class ScenarioMode
{
    public const FIRE_AND_FORGET = 'fire-and-forget';
    public const BATCH_CONFIRM = 'batch-confirm';
    public const AUTO_ACK = 'auto-ack';
    public const LARAVEL_DISPATCH = 'laravel-dispatch';
    public const LARAVEL_WORKER = 'laravel-worker';
    // Issue #368 comparison: blind batched fill + push-consume with manual
    // per-message ACK and prefetch 128 on every driver (drain phase measured).
    public const SUSTAINED_CONSUME = 'sustained-consume';
}
