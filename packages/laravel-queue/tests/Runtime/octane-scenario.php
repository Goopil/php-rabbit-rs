<?php

declare(strict_types=1);

/**
 * Shell-invokable driver for the Octane runtime certification scenario.
 *
 * Usage: php octane-scenario.php <command> [args]
 *   declare                        declare the scenario queue (idempotent)
 *   purge                          remove + re-declare the scenario queue
 *   publish <count>                POST /publish count times through the server
 *   wait-depth <expected> <timeout-seconds>
 *   wait-buffered <expected> <timeout-seconds>
 *   consume-ack <count> <timeout-seconds>
 *
 * Exits non-zero with a "FAIL: ..." message on any assertion failure.
 * The entry file deliberately lives outside the PSR-4 map and requires the
 * assertions class directly (no autoloader needed on the harness machine).
 */

use Goopil\RabbitRs\Laravel\Tests\Runtime\Scenario;

require_once __DIR__.'/Scenario.php';

$command = $argv[1] ?? '';
$args = array_slice($argv, 2);

$fail = static function (string $message): never {
    fwrite(STDERR, "FAIL: {$message}\n");
    exit(1);
};

try {
    switch ($command) {
        case 'declare':
            Scenario::declareQueue();
            break;

        case 'purge':
            Scenario::purge();
            break;

        case 'publish':
            Scenario::publish((int) $args[0]);
            break;

        case 'wait-depth':
            Scenario::waitDepth((int) $args[0], (int) $args[1]);
            break;

        case 'wait-buffered':
            Scenario::waitBuffered((int) $args[0], (int) $args[1]);
            break;

        case 'consume-ack':
            Scenario::consumeAck((int) $args[0], (int) $args[1]);
            break;

        default:
            $fail("unknown scenario command '{$command}'");
    }
} catch (Throwable $e) {
    $fail($e->getMessage());
}

echo "ok: {$command}\n";
