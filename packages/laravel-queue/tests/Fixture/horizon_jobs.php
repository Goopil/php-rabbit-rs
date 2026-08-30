<?php

declare(strict_types=1);

namespace Fixtures;

use Illuminate\Bus\Queueable;
use Illuminate\Contracts\Queue\ShouldQueue;

/**
 * Queueable fixtures for the Horizon dispatch-path tests.
 *
 * They are plain classes (no extension needed) resolved at runtime by the
 * bus dispatcher, mirroring how application jobs reach the queue connection.
 */
class CommitJob implements ShouldQueue
{
    use Queueable;

    public function __construct()
    {
        $this->afterCommit();
    }
}

class BulkJob implements ShouldQueue
{
    use Queueable;
}
