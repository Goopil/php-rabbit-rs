<?php

declare(strict_types=1);

/**
 * Helper functions for the worker stub script.
 */

namespace {
    const WORKER_PREFIX = '/worker-';

    /**
     * Atomically increment and return the invocation count for a worker.
     */
    function recordInvocation(string $stateDir, int $worker): int
    {
        if (! is_dir($stateDir)) {
            @mkdir($stateDir, 0o777, true);
        }

        $counterFile = $stateDir . WORKER_PREFIX . $worker . '-count.txt';
        $current = 0;
        if (is_file($counterFile)) {
            $content = file_get_contents($counterFile);
            if ($content !== false && $content !== '') {
                $current = (int) $content;
            }
        }
        $current++;
        file_put_contents($counterFile, (string) $current, LOCK_EX);

        return $current;
    }

    /**
     * Write a marker file indicating that the worker stub has started for
     * this invocation. Tests poll for this file to know when the child has
     * fully launched.
     */
    function writeWorkerMarker(string $stateDir, int $worker, int $invocation): void
    {
        if (! is_dir($stateDir)) {
            @mkdir($stateDir, 0o777, true);
        }

        $markerFile = $stateDir . WORKER_PREFIX . $worker . '-started.txt';
        file_put_contents(
            $markerFile,
            json_encode([
                'worker' => $worker,
                'invocation' => $invocation,
                'pid' => getmypid(),
                'time' => microtime(true),
            ]),
            LOCK_EX,
        );
    }

    /**
     * Write a marker file indicating that the worker stub has exited.
     * Tests poll for this file to verify the supervisor stopped a running
     * worker rather than leaving it as an orphan.
     */
    function writeWorkerExitMarker(string $stateDir, int $worker): void
    {
        if (! is_dir($stateDir)) {
            @mkdir($stateDir, 0o777, true);
        }

        $exitFile = $stateDir . WORKER_PREFIX . $worker . '-exited.txt';
        file_put_contents(
            $exitFile,
            json_encode([
                'worker' => $worker,
                'pid' => getmypid(),
                'time' => microtime(true),
            ]),
            LOCK_EX,
        );
    }
}
