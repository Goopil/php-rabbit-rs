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
    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * The $config array follows the normalized native configuration schema.
     * The optional `consumer.wait_timeout` key (integer milliseconds,
     * default 30000, bounded 1000..86400000) caps how long consumer()
     * blocks while a broker connection becomes ready before failing with a
     * ConnectionException.
     */
    public function __construct(array $config)
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Publishes one message, returning its stable message identifier.
     *
     * @param array{broker: string, exchange: string, routing_key: string, payload: string, message_id: string, content_type?: string, correlation_id?: string, delay_ms?: int, timeout_ms?: int, headers?: array<string, bool|int|float|string|null>} $message
     *
     * Payload and all headers are limited to 1 MiB and 64 KiB per call respectively.
     * Headers are flat, contain at most 128 entries, and timeout_ms is between 1 and 86,400,000.
     *
     * @throws \Goopil\RabbitRs\BackpressureException when the bounded publish
     *   buffer is full (outage with sustained traffic); retry with the same
     *   message later. Already-buffered messages are never dropped.
     */
    public function publish(array $message): string
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
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
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     */
    public function consumer(string $profile): Consumer
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     *
     * Flushes the publish buffer, sending all buffered messages to the broker.
     *
     * In blind mode this is a barrier: every request enqueued on the publish
     * pump before this call has been handed to the transport (or dropped for
     * lack of a channel during recovery) when flush() returns. Hand-off is
     * not delivery: per the blind fire-and-forget contract, a later transport
     * failure is a silent loss.
     */
    public function flush(): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     *
     * @return array{closed: bool, pid: int, handle: string, publishes_total: int, confirmations_total: int, returns_total: int, backpressure_total: int, reconnects_total: int, duplicates_total: int}
     */
    public function stats(): array
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     */
    public function size(string $broker, string $queue): int
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     */
    public function clear(string $broker, string $queue): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Registers a PHP callback invoked when a broker connection state changes.
     *
     * The callback receives (string $broker, string $state, int $generation).
     * It is invoked synchronously on the PHP thread during publish, consume,
     * and stats() operations.
     *
     * @param callable(string, string, int): void $callback
     */
    public function onConnectionState(callable $callback): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Registers a PHP callback invoked when publisher backpressure is detected.
     *
     * The callback receives (string $broker, int $inFlight, int $capacity).
     * It is invoked synchronously on the PHP thread during publish, consume,
     * and stats() operations.
     *
     * @param callable(string, int, int): void $callback
     */
    public function onBackpressure(callable $callback): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     *
     * Removes every registered event callback, returning how many were
     * removed (connection-state and backpressure combined).
     *
     * Connections sharing one native pool each register their own callbacks;
     * clearing allows a fresh registration to start from a clean slate.
     */
    public function clearEventCallbacks(): int
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
     */
    public function close(): void
    {
    }
}

final class Consumer
{
    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     */
    public function next(int $timeoutMs): ?Delivery
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     *
     * Returns the next delivery without blocking, or null when the buffer is empty.
     */
    public function tryNext(): ?Delivery
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Drains up to $max deliveries from the buffer in one call.
     *
     * When the buffer is empty, blocks up to $timeoutMs for the first delivery,
     * then drains any remaining deliveries that became available.
     * $max is clamped to 1..=256.
     *
     * @return list<Delivery>
     */
    public function nextBatch(int $max, int $timeoutMs): array
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Acknowledges a contiguous prefix of deliveries up to and including the
     * given delivery using a single AMQP basic.ack with multiple=true.
     * Fire-and-forget: enqueues the command and returns immediately.
     */
    public function ackThrough(Delivery $delivery): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Acknowledges a batch of deliveries, potentially across different channels.
     * Fire-and-forget: enqueues each settlement command without blocking.
     * Bounded to 256 deliveries per call.
     *
     * @param list<Delivery> $deliveries
     */
    public function ackBatch(array $deliveries): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     *
     * Drains settlement errors that have surfaced asynchronously since the
     * last call. Each entry contains delivery_tag, subscription, error_kind,
     * and message.
     *
     * @return list<array{delivery_tag: int, subscription: string, error_kind: string, message: string}>
     */
    public function drainErrors(): array
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Consumer Method is provided by the C extension at runtime.
     */
    public function close(): void
    {
    }
}

final class Delivery
{
    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     */
    public function payload(): string
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     *
     * @return array{message_id: string, correlation_id?: string, subscription: string, attempts: int, state: string, headers: array<string, bool|int|float|string|null>}
     *
     * Nested broker headers such as x-death are omitted from the flat PHP header model.
     */
    public function metadata(): array
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     *
     * Returns the AMQP delivery tag.
     */
    public function deliveryTag(): int
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     *
     * Acknowledges the delivery (fire-and-forget with bounded backpressure).
     */
    public function ack(): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Releases the delivery immediately or after a delay (fire-and-forget).
     */
    public function release(int $delayMs = 0): void
    {
    }

    /**
     * Implemented by the ext-rabbit_rs native extension.
     * @see \Goopil\RabbitRs\Delivery Method is provided by the C extension at runtime.
     * @noinspection PhpUnusedParameterInspection
     *
     * Rejects the delivery with optional requeueing (fire-and-forget).
     */
    public function reject(bool $requeue = false): void
    {
    }
}
