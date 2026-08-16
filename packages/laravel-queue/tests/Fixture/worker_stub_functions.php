<?php

declare(strict_types=1);

/**
 * Helper functions for the worker stub script.
 */

namespace {
    /**
     * Atomically increment and return the invocation count for a worker.
     */
    function recordInvocation(string $stateDir, int $worker): int
    {
        if (! is_dir($stateDir)) {
            @mkdir($stateDir, 0o777, true);
        }

        $counterFile = $stateDir . '/worker-' . $worker . '-count.txt';
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

        $markerFile = $stateDir . '/worker-' . $worker . '-started.txt';
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
}
