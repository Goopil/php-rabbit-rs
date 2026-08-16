<?php

declare(strict_types=1);

/**
 * Minimal example job for Rabbit RS.
 *
 * Dispatch with:
 *   ProcessOrder::dispatch(42);
 *
 * Consume with:
 *   php artisan queue:work --connection=rabbit-rs
 */

namespace App\Jobs;

use Illuminate\Bus\Queueable;
use Illuminate\Contracts\Queue\ShouldQueue;
use Illuminate\Foundation\Bus\Dispatchable;
use Illuminate\Queue\InteractsWithQueue;
use Illuminate\Queue\SerializesModels;
use Illuminate\Support\Facades\Log;

class ProcessOrder implements ShouldQueue
{
    use Dispatchable;
    use InteractsWithQueue;
    use Queueable;
    use SerializesModels;

    public int $tries = 3;

    public int $backoff = 5;

    public function __construct(
        public readonly int $orderId,
    ) {}

    public function handle(): void
    {
        Log::info("Processing order {$this->orderId}");

        // Your business logic here
        // Order::find($this->orderId)->process();

        Log::info("Order {$this->orderId} processed");
    }

    /**
     * Handle a job failure.
     */
    public function failed(\Throwable $exception): void
    {
        Log::error("Order {$this->orderId} failed: {$exception->getMessage()}");
    }
}
