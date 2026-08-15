<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Events;

use Illuminate\Broadcasting\InteractsWithSockets;
use Illuminate\Foundation\Events\Dispatchable;
use Illuminate\Queue\SerializesModels;

final class BackpressureDetected
{
    use Dispatchable;
    use InteractsWithSockets;
    use SerializesModels;

    public function __construct(
        public readonly string $broker,
        public readonly int $inFlight,
        public readonly int $capacity,
    ) {}
}
