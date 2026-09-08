<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Events;

use Illuminate\Broadcasting\InteractsWithSockets;
use Illuminate\Foundation\Events\Dispatchable;
use Illuminate\Queue\SerializesModels;

/**
 * Dispatched by rabbit-rs:probe before the exit code is decided. Synchronous
 * listeners may flip $verdict to force a probe to fail (maintenance mode,
 * external flags) and pull the pod out of rotation.
 */
final class RabbitRsProbeEvaluated
{
    use Dispatchable;
    use InteractsWithSockets;
    use SerializesModels;

    public function __construct(
        public readonly string $probe,
        public readonly string $state,
        public bool $verdict = true,
    ) {}
}
