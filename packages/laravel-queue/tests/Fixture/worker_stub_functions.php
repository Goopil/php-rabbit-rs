<?php

declare(strict_types=1);

/**
 * Helper functions for the worker stub script.
 */

namespace {
    const WORKER_PREFIX = '/worker-';

    /**
     * Atomically replace the content of a state file.
     *
     * file_put_contents() truncates the target at open, before the write:
     * a reader (or a SIGTERM interrupting the writer) can observe or leave
     * behind an empty file. Writing to a temp file in the same directory and
     * renaming over the target removes that window entirely — rename(2) is
     * atomic, so readers see either the old or the new complete content.
     * An interrupted writer may leave an orphan temp file, which the tests'
     * state-dir cleanup removes.
     */
    function atomicWrite(string $path, string $content): void
    {
        $tmp = $path.'.'.getmypid().'.tmp';
        file_put_contents($tmp, $content);
        rename($tmp, $path);
    }

    /**
     * Atomically increment and return the invocation count for a worker.
     */
    function recordInvocation(string $stateDir, int $worker): int
    {
        if (! is_dir($stateDir)) {
            @mkdir($stateDir, 0o777, true);
        }

        $counterFile = $stateDir.WORKER_PREFIX.$worker.'-count.txt';
        $current = 0;
        if (is_file($counterFile)) {
            $content = file_get_contents($counterFile);
            if ($content !== false && $content !== '') {
                $current = (int) $content;
            }
        }
        $current++;
        atomicWrite($counterFile, (string) $current);

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

        $markerFile = $stateDir.WORKER_PREFIX.$worker.'-started.txt';
        atomicWrite(
            $markerFile,
            (string) json_encode([
                'worker' => $worker,
                'invocation' => $invocation,
                'pid' => getmypid(),
                'time' => microtime(true),
            ]),
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

        $exitFile = $stateDir.WORKER_PREFIX.$worker.'-exited.txt';
        atomicWrite(
            $exitFile,
            (string) json_encode([
                'worker' => $worker,
                'pid' => getmypid(),
                'time' => microtime(true),
            ]),
        );
    }
}
