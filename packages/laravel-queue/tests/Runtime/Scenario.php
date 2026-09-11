<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Tests\Runtime;

use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\RabbitRsConnections;
use Illuminate\Contracts\Console\Kernel as ConsoleKernel;
use Illuminate\Support\Facades\Queue;
use RuntimeException;

/**
 * Scenario assertions for the real-server Octane certification harness
 * (scripts/test-octane-runtime.sh).
 *
 * Each method is one shell-invokable step driven over HTTP: publications go
 * through the RUNNING Octane server (POST /publish), buffer occupancy through
 * GET /stats, and queue depth through the RabbitMQ management API. Deadline
 * polling lives here; process lifecycle stays in the shell script.
 *
 * Configuration arrives through environment variables set by the harness:
 *   RUNTIME_BASE_URL   Octane app base URL (default http://127.0.0.1:8180)
 *   RUNTIME_MGMT_URL   RabbitMQ management API (default http://localhost:15672)
 *   RUNTIME_MGMT_USER  management user (default admin)
 *   RUNTIME_MGMT_PASS  management password (default admin_lab)
 *   RUNTIME_QUEUE      scenario queue name
 *   RUNTIME_VHOST      scenario vhost (default /orders-eu)
 */
final class Scenario
{
    public static function baseUrl(): string
    {
        return rtrim((string) (getenv('RUNTIME_BASE_URL') ?: 'http://127.0.0.1:8180'), '/');
    }

    public static function queue(): string
    {
        $queue = getenv('RUNTIME_QUEUE');

        if (! is_string($queue) || $queue === '') {
            throw new RuntimeException('RUNTIME_QUEUE is not set');
        }

        return $queue;
    }

    public static function vhost(): string
    {
        return (string) (getenv('RUNTIME_VHOST') ?: '/orders-eu');
    }

    /**
     * Declares the scenario queue (idempotent): quorum + durable, matching
     * the connection's topology defaults.
     */
    public static function declareQueue(): void
    {
        [$status, $body] = self::management('PUT', self::queueUrl(), (string) json_encode([
            'durable' => true,
            'arguments' => ['x-queue-type' => 'quorum'],
        ]));

        if (! in_array($status, [201, 204], true)) {
            throw new RuntimeException("declare queue failed: HTTP {$status} {$body}");
        }
    }

    /**
     * Removes and re-declares the scenario queue for a clean slate.
     */
    public static function purge(): void
    {
        self::management('DELETE', self::queueUrl());
        self::declareQueue();
    }

    /**
     * Drives POST /publish exactly $count times, asserting each dispatch is
     * accepted by the running Octane worker.
     */
    public static function publish(int $count): void
    {
        for ($i = 1; $i <= $count; $i++) {
            [$status, $body] = self::http('POST', self::baseUrl().'/publish');

            if ($status !== 200) {
                throw new RuntimeException("publish {$i}/{$count} failed: HTTP {$status} {$body}");
            }

            $decoded = json_decode($body, true);
            if (($decoded['published'] ?? null) !== true) {
                throw new RuntimeException("publish {$i}/{$count} unexpected response: {$body}");
            }
        }
    }

    /**
     * Polls the management API until the scenario queue depth (ready +
     * unacked messages) equals $expected, within the deadline.
     */
    public static function waitDepth(int $expected, int $timeoutSeconds): void
    {
        self::waitUntil("queue depth == {$expected}", $timeoutSeconds, static fn (): int => self::depth(), $expected);
    }

    /**
     * Polls GET /stats until the worker's publish buffer occupancy equals
     * $expected, within the deadline.
     */
    public static function waitBuffered(int $expected, int $timeoutSeconds): void
    {
        self::waitUntil("publish_buffered == {$expected}", $timeoutSeconds, static fn (): int => self::buffered(), $expected);
    }

    /**
     * Consumes and acknowledges exactly $count jobs through GET /consume-one
     * within the deadline.
     */
    public static function consumeAck(int $count, int $timeoutSeconds): void
    {
        $deadline = microtime(true) + $timeoutSeconds;
        $acked = 0;

        while ($acked < $count) {
            if (microtime(true) > $deadline) {
                throw new RuntimeException("consume-ack: acknowledged {$acked}/{$count} before the deadline");
            }

            [$status, $body] = self::http('GET', self::baseUrl().'/consume-one');
            if ($status !== 200) {
                throw new RuntimeException("consume-one failed: HTTP {$status} {$body}");
            }

            $decoded = json_decode($body, true);
            if (($decoded['acked'] ?? null) === true) {
                $acked++;
            } else {
                usleep(250_000);
            }
        }
    }

    /**
     * Consumes and acknowledges exactly $count jobs from this CLI process
     * (the queue worker shape: pop + ack in one long-lived process, no HTTP).
     *
     * The harness consumes through this path instead of the running server's
     * /consume-one: the driver closes cached consumers after every request
     * (Octane terminating hook) while the native client keeps serving the
     * closed handle from its per-profile cache, so server-side pops fail
     * from the second request on (driver bug, see the WS5b report).
     * Consumption in production goes through CLI workers, which this path
     * mirrors exactly.
     */
    public static function consumeAckCli(int $count, int $timeoutSeconds): void
    {
        $deadline = microtime(true) + $timeoutSeconds;
        $acked = 0;

        $app = self::app();
        $connection = (string) config('queue.default');

        while ($acked < $count) {
            if (microtime(true) > $deadline) {
                throw new RuntimeException("consume-ack-cli: acknowledged {$acked}/{$count} before the deadline");
            }

            $job = Queue::connection($connection)->pop();
            if ($job === null) {
                usleep(250_000);

                continue;
            }

            $job->delete();
            $acked++;
        }

        // Settlements are fire-and-forget: the ack commands queue on the
        // consumer actor and flush asynchronously, so the CLI process must
        // stay alive pumping until the broker confirms the drain — exiting
        // right after the last ack would drop still-queued acks with the
        // runtime (at-least-once: the messages survive, they just return to
        // ready state).
        do {
            if (self::depth() === 0) {
                return;
            }

            if (microtime(true) > $deadline) {
                throw new RuntimeException('consume-ack-cli: acks not fully flushed before the deadline (depth '.self::depth().')');
            }

            Queue::connection($connection)->pop();
            usleep(100_000);
        } while (true);
    }

    /**
     * @return int the current queue depth (messages ready + unacked)
     */
    public static function depth(): int
    {
        // Queried through the extension's Pool::size() (a passive AMQP queue
        // declare) instead of the management API: the management endpoint may
        // omit the `messages` gauge entirely until its stats collector emits
        // one, which made deadline polling read 0 forever on a fresh lab.
        $app = self::app();
        $connection = (string) config('queue.default');

        $config = (array) config("queue.connections.{$connection}");
        $compiled = ConnectionCompiler::compile($connection, $config, RabbitRsConnections::packageDefaults());
        $pool = $app->make(NativePoolFactory::class)->make($compiled['native']);

        // Deliberately NOT closed: the factory caches the pool instance for
        // the deadline-polling loop, and this CLI process exits right after.
        return $pool->size($connection, self::queue());
    }

    /**
     * @return int the worker's current publish buffer occupancy
     */
    public static function buffered(): int
    {
        [$status, $body] = self::http('GET', self::baseUrl().'/stats');

        if ($status !== 200) {
            throw new RuntimeException("stats failed: HTTP {$status} {$body}");
        }

        $decoded = json_decode($body, true);
        $buffered = $decoded['publish_buffered'] ?? null;

        if (! is_int($buffered)) {
            throw new RuntimeException("stats missing publish_buffered: {$body}");
        }

        return $buffered;
    }

    /**
     * @return array{0: int, 1: string} [HTTP status, response body]
     */
    private static function http(string $method, string $url, ?string $payload = null): array
    {
        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, $method);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_CONNECTTIMEOUT_MS, 2000);
        curl_setopt($ch, CURLOPT_TIMEOUT_MS, 15000);
        if ($payload !== null) {
            curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
            curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        }
        $body = curl_exec($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        $error = curl_error($ch);
        if ($body === false) {
            throw new RuntimeException("request {$method} {$url} failed: {$error}");
        }

        return [$status, (string) $body];
    }

    /**
     * @return array{0: int, 1: string} [HTTP status, response body]
     */
    private static function management(string $method, string $url, ?string $payload = null): array
    {
        $ch = curl_init($url);
        curl_setopt($ch, CURLOPT_CUSTOMREQUEST, $method);
        curl_setopt($ch, CURLOPT_RETURNTRANSFER, true);
        curl_setopt($ch, CURLOPT_USERPWD, self::managementUser().':'.self::managementPassword());
        curl_setopt($ch, CURLOPT_CONNECTTIMEOUT_MS, 2000);
        curl_setopt($ch, CURLOPT_TIMEOUT_MS, 15000);
        if ($payload !== null) {
            curl_setopt($ch, CURLOPT_POSTFIELDS, $payload);
            curl_setopt($ch, CURLOPT_HTTPHEADER, ['Content-Type: application/json']);
        }
        $body = curl_exec($ch);
        $status = (int) curl_getinfo($ch, CURLINFO_RESPONSE_CODE);
        if ($body === false) {
            throw new RuntimeException("management {$method} {$url} failed: is the lab up?");
        }

        return [$status, (string) $body];
    }

    private static function managementUser(): string
    {
        return (string) (getenv('RUNTIME_MGMT_USER') ?: 'admin');
    }

    private static function managementPassword(): string
    {
        return (string) (getenv('RUNTIME_MGMT_PASS') ?: 'admin_lab');
    }

    private static ?\Illuminate\Foundation\Application $app = null;

    /**
     * Boots the scenario app (cached per process) so depth() compiles the
     * SAME connection config the Octane worker uses.
     */
    private static function app(): \Illuminate\Foundation\Application
    {
        if (self::$app instanceof \Illuminate\Foundation\Application) {
            return self::$app;
        }

        $appRoot = __DIR__.'/app';

        require $appRoot.'/vendor/autoload.php';

        $app = require $appRoot.'/bootstrap/app.php';
        $app->make(ConsoleKernel::class)->bootstrap();

        return self::$app = $app;
    }

    private static function queueUrl(): string
    {
        $base = rtrim((string) (getenv('RUNTIME_MGMT_URL') ?: 'http://localhost:15672'), '/');

        return $base.'/api/queues/'.rawurlencode(self::vhost()).'/'.rawurlencode(self::queue());
    }

    /**
     * Polls $observe until it returns $expected or the deadline expires,
     * logging the observed value on every miss for diagnosability.
     *
     * @param  callable(): int  $observe
     */
    private static function waitUntil(string $expectation, int $timeoutSeconds, callable $observe, int $expected): void
    {
        $deadline = microtime(true) + $timeoutSeconds;

        do {
            $observed = $observe();
            if ($observed === $expected) {
                return;
            }

            error_log("waiting: {$expectation} (observed {$observed})");
            usleep(250_000);
        } while (microtime(true) < $deadline);

        throw new RuntimeException("deadline exceeded waiting for {$expectation}");
    }
}
