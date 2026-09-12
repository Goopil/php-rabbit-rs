<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

/**
 * One scaling decision for a connection: how many children to add and how
 * many to stop. Both are zero when no action applies.
 */
final class ScaleAction
{
    public function __construct(
        public readonly int $up = 0,
        public readonly int $down = 0,
    ) {}
}
