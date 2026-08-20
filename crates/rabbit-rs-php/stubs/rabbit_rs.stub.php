<?php

declare(strict_types=1);

namespace Goopil\RabbitRs;

class Exception extends \Exception
{
}

final class BackpressureException extends Exception
{
}

final class ConnectionException extends Exception
{
}

final class Pool
{
    public function __construct(array $config)
    {
    }

    /**
     * @param array{broker: string, exchange: string, routing_key: string, payload: string, message_id: string, content_type?: string, correlation_id?: string, delay_ms?: int, timeout_ms?: int, headers?: array<string, bool|int|float|string|null>} $message
     *
     * Payload and all headers are limited to 1 MiB and 64 KiB per call respectively.
     * Headers are flat, contain at most 128 entries, and timeout_ms is between 1 and 86,400,000.
     */
    public function publish(array $message): string
    {
    }

    /**
     * @param list<array{broker: string, exchange: string, routing_key: string, payload: string, message_id: string, content_type?: string, correlation_id?: string, delay_ms?: int, timeout_ms?: int, headers?: array<string, bool|int|float|string|null>}> $messages
     * @return list<string>
     *
     * A batch contains at most 256 messages and 1 MiB of cumulative payload.
     * Header count and size limits are cumulative across the complete call.
     */
    public function publishBatch(array $messages): array
    {
    }

    /**
     * Flushes the publish buffer, sending all buffered messages to the broker.
     */
    public function flush(): void
    {
    }

    public function consumer(string $profile): Consumer
    {
    }

    /**
     * @return array{
     *   closed: bool,
     *   pid: int,
     *   handle: string,
     *   publishes_total: int,
     *   confirmations_total: int,
     *   returns_total: int,
     *   backpressure_total: int,
     *   reconnects_total: int,
     *   deliveries_total: int,
     *   acks_total: int,
     *   rejects_total: int,
     *   confirmation_latency_p50: int,
     *   confirmation_latency_p95: int,
     *   confirmation_latency_p99: int,
     *   settlement_latency_p50: int,
     *   settlement_latency_p95: int,
     *   settlement_latency_p99: int
     * }
     */
    public function stats(): array
    {
    }

    public function size(string $broker, string $queue): int
    {
    }

    public function clear(string $broker, string $queue): void
    {
    }

    /**
     * Registers a PHP callback invoked when a broker connection state changes.
     *
     * The callback receives (string $broker, string $state, int $generation).
     * It is invoked synchronously on the PHP thread during stats().
     *
     * @param callable(string, string, int): void $callback
     */
    public function onConnectionState(callable $callback): void
    {
    }

    /**
     * Registers a PHP callback invoked when publisher backpressure is detected.
     *
     * The callback receives (string $broker, int $inFlight, int $capacity).
     * It is invoked synchronously on the PHP thread during stats().
     *
     * @param callable(string, int, int): void $callback
     */
    public function onBackpressure(callable $callback): void
    {
    }

    public function close(): void
    {
    }
}

final class Consumer implements \IteratorAggregate
{
    public function next(int $timeoutMs): ?Delivery
    {
    }

    /**
     * Returns the next delivery without blocking, or null when the buffer is empty.
     */
    public function tryNext(): ?Delivery
    {
    }

    /**
     * Processes messages by calling the given callback for each delivery.
     *
     * @param callable(Delivery): bool $handler
     * @param int $count Number of messages to process (0 = unlimited)
     * @param int $timeoutMs Total timeout in milliseconds
     * @return int Number of messages processed
     */
    public function consume(callable $handler, int $count = 0, int $timeoutMs = 1000): int
    {
    }

    /**
     * Returns an iterator for use in foreach loops.
     */
    public function getIterator(): \Iterator
    {
    }

    public function close(): void
    {
    }
}

/**
 * Iterator for the Consumer class, implementing PHP's Iterator interface.
 *
 * Uses try_next() (non-blocking) on the fast path and next() with a
 * default 1-second timeout on the slow path.
 */
final class ConsumerIterator implements \Iterator
{
    public function current(): ?Delivery
    {
    }

    public function key(): int
    {
    }

    public function next(): void
    {
    }

    public function rewind(): void
    {
    }

    public function valid(): bool
    {
    }
}

final class Delivery
{
    public function payload(): string
    {
    }

    /**
     * @return array{message_id: string, correlation_id?: string, subscription: string, attempts: int, state: string, headers: array<string, bool|int|float|string|null>}
     *
     * Nested broker headers such as x-death are omitted from the flat PHP header model.
     */
    public function metadata(): array
    {
    }

    public function ack(): void
    {
    }

    public function release(int $delayMs = 0): void
    {
    }

    public function reject(bool $requeue = false): void
    {
    }
}
