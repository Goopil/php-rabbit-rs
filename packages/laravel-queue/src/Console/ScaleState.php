<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

/**
 * Per-connection scaling bookkeeping mutated by {@see WorkScalePolicy::decide()}:
 * one instance per work-plan entry, held by the supervisor for the whole run.
 */
final class ScaleState
{
    /** Unix timestamp (microseconds) of the last scale action for this connection. */
    public float $lastScaleAt = 0.0;

    /**
     * When the current continuous zero-depth streak started; null while the
     * depth is above zero.
     */
    public ?float $emptySince = null;
}
