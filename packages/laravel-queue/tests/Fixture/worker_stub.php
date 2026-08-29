<?php

declare(strict_types=1);

/**
 * Minimal worker stub for WorkerSupervisor end-to-end tests.
 *
 * The supervisor spawns this script as a child process instead of `queue:work`
 * so that tests are deterministic, fast, and do not require a full Laravel stack.
 *
 * Behaviour is controlled by environment variables:
 *
 *   RABBIT_RS_WORKER          Worker index assigned by the supervisor.
 *   RABBIT_RS_STUB_MODE       One of: run, exit-clean, crash, crash-after.
 *                             Defaults to "run".
 *   RABBIT_RS_STUB_CRASH_AFTER Number of invocations before crashing. Used with
 *                             "crash-after" mode. Defaults to 0 (never).
 *   RABBIT_RS_STUB_STATE_DIR  Directory where per-worker state files are written.
 *
 * In "run" mode the script waits for SIGTERM/SIGINT and exits with 0.
 * In "exit-clean" mode it exits immediately with 0.
 * In "crash" mode it exits immediately with 1.
 * In "crash-after" mode it exits with 1 only after N prior invocations; before
 * that it runs until signaled.
 */

namespace {
    require_once __DIR__ . '/worker_stub_functions.php';

    if (isset($_ENV['RABBIT_RS_WORKER'])) {
        $worker = (string) $_ENV['RABBIT_RS_WORKER'];
    } elseif (getenv('RABBIT_RS_WORKER') !== false) {
        $worker = (string) getenv('RABBIT_RS_WORKER');
    } else {
        $worker = '0';
    }
    if (isset($_ENV['RABBIT_RS_STUB_MODE'])) {
        $mode = (string) $_ENV['RABBIT_RS_STUB_MODE'];
    } elseif (getenv('RABBIT_RS_STUB_MODE') !== false) {
        $mode = (string) getenv('RABBIT_RS_STUB_MODE');
    } else {
        $mode = 'run';
    }
    if (isset($_ENV['RABBIT_RS_STUB_CRASH_AFTER'])) {
        $crashAfter = (int) $_ENV['RABBIT_RS_STUB_CRASH_AFTER'];
    } elseif (getenv('RABBIT_RS_STUB_CRASH_AFTER') !== false) {
        $crashAfter = (int) getenv('RABBIT_RS_STUB_CRASH_AFTER');
    } else {
        $crashAfter = 0;
    }
    if (isset($_ENV['RABBIT_RS_STUB_STATE_DIR'])) {
        $stateDir = (string) $_ENV['RABBIT_RS_STUB_STATE_DIR'];
    } elseif (getenv('RABBIT_RS_STUB_STATE_DIR') !== false) {
        $stateDir = (string) getenv('RABBIT_RS_STUB_STATE_DIR');
    } else {
        $stateDir = sys_get_temp_dir();
    }

    $invocation = recordInvocation($stateDir, (int) $worker);
    writeWorkerMarker($stateDir, (int) $worker, $invocation);

    if ($mode === 'exit-clean') {
        exit(0);
    }

    if ($mode === 'crash') {
        exit(1);
    }

    if ($mode === 'crash-after' && $invocation > $crashAfter) {
        exit(1);
    }

    // "run" or "crash-after" before threshold: wait for signal.
    $running = true;
    if (function_exists('pcntl_async_signals')) {
        pcntl_async_signals(true);
        pcntl_signal(SIGTERM, static function () use (&$running): void {
            $running = false;
        });
        pcntl_signal(SIGINT, static function () use (&$running): void {
            $running = false;
        });
    }

    while ($running) {
        usleep(50_000);
    }

    writeWorkerExitMarker($stateDir, (int) $worker);

    exit(0);
}
