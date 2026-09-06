<?php

// Stubs for rabbit_rs

namespace Goopil\RabbitRs {
    class BackpressureException extends Goopil\RabbitRs\Exception {
        public function __construct() {}
    }

    class ConnectionException extends Goopil\RabbitRs\Exception {
        public function __construct() {}

        /**
         * Throws a connection exception carrying the given message; never returns.
         *
         * A native exception's message can only be set when the exception is
         * thrown (the base PHP exception message is written by the throw
         * machinery), so PHP userland that must surface a connection-level
         * failure itself — e.g. the Laravel queue draining an async settlement
         * error — calls this factory instead of constructing the class.
         *
         * @param string $message
         * @return void
         */
        public static function throw(string $message): void {}
    }

    /**
     * Native consumer for an aggregated subscription profile.
     */
    class Consumer {
        public function __construct() {}

        /**
         * Closes the consumer handle when PHP garbage-collects the object.
         *
         * This is a best-effort safety net that prevents AMQP channel leaks in
         * long-lived processes (Octane, daemons) when `close()` is never called
         * explicitly. The underlying `ConsumerHandle::Drop` also sends `Close` to
         * the actor so channels are closed even if PHP never calls `close()`.
         */
        public function __destruct() {}

        /**
         * Acknowledges a batch of deliveries across potentially different channels.
         *
         * Fire-and-forget: enqueues each settlement command without blocking.
         * Bounded to 256 deliveries per call. The cap is checked before any
         * settlement is enqueued so a rejected call has no side effects
         * (audit F-20).
         *
         * @param list<\Goopil\RabbitRs\Delivery> $deliveries
         *
         * @param array $deliveries
         * @return void
         */
        public function ackBatch(array $deliveries): void {}

        /**
         * Acknowledges a contiguous prefix of deliveries up to and including the
         * given delivery using a single AMQP `basic.ack` with `multiple=true`.
         *
         * Fire-and-forget: enqueues the command and returns immediately.
         *
         * @param \Goopil\RabbitRs\Delivery $delivery
         * @return void
         */
        public function ackThrough(\Goopil\RabbitRs\Delivery $delivery): void {}

        /**
         * Closes this consumer handle.
         *
         * @return void
         */
        public function close(): void {}

        /**
         * Drains settlement errors that have surfaced asynchronously since the
         * last call. Returns an array of error hashes, each containing
         * `delivery_tag`, `subscription`, `error_kind`, and `message`.
         *
         * @return list<array{delivery_tag: int, subscription: string,
         *   error_kind: string, message: string}>
         *
         * @return array
         */
        public function drainErrors(): array {}

        /**
         * Returns the next delivery within the requested timeout.
         *
         * The fast path checks the lock-free buffer without crossing into the
         * async runtime. The slow path blocks on the async runtime with the
         * specified timeout.
         *
         * @param int $timeoutMs
         * @return \Goopil\RabbitRs\Delivery|null
         */
        public function next(int $timeoutMs): ?\Goopil\RabbitRs\Delivery {}

        /**
         * Drains up to `max` deliveries from the buffer in one call.
         *
         * The fast path checks the lock-free buffer without crossing into the
         * async runtime. When the buffer is empty, the slow path blocks on the
         * async runtime with the specified timeout, then drains whatever is
         * available. `max` is clamped to `1..=256`.
         *
         * @return list<\Goopil\RabbitRs\Delivery>
         *
         * @param int $max
         * @param int $timeoutMs
         * @return array
         */
        public function nextBatch(int $max, int $timeoutMs): array {}

        /**
         * Attempts to return the next delivery without blocking.
         *
         * Returns `Some(Delivery)` when one is available in the buffer,
         * or `None` when the buffer is empty. No timeout, no async wait.
         *
         * @return \Goopil\RabbitRs\Delivery|null
         */
        public function tryNext(): ?\Goopil\RabbitRs\Delivery {}
    }

    /**
     * Native delivery and its acknowledgement token.
     */
    class Delivery {
        public function __construct() {}

        /**
         * Acknowledges the delivery (fire-and-forget with bounded backpressure).
         *
         * @return void
         */
        public function ack(): void {}

        /**
         * Returns the AMQP delivery tag.
         *
         * @return int
         */
        public function deliveryTag(): int {}

        /**
         * Returns delivery metadata as a PHP array.
         *
         * @return array{message_id: string, correlation_id?: string,
         *   subscription: string, attempts: int, state: string,
         *   headers: array<string, bool|int|float|string|null>}
         *
         * Nested broker headers such as `x-death` are omitted from the flat PHP
         * header model.
         *
         * @return array
         */
        public function metadata(): array {}

        /**
         * Returns the binary-safe delivery payload.
         *
         * @return string
         */
        public function payload(): string {}

        /**
         * Rejects the delivery with optional requeueing (fire-and-forget).
         *
         * @param bool $requeue
         * @return void
         */
        public function reject(bool $requeue = false): void {}

        /**
         * Releases the delivery immediately or after a delay (fire-and-forget).
         *
         * @param int $delayMs
         * @return void
         */
        public function release(int $delayMs = 0): void {}
    }

    class Exception extends \Exception {
        public function __construct() {}
    }

    /**
     * Native `RabbitMQ` connection and operation pool.
     */
    class Pool {
        /**
         * Creates a native pool from its PHP configuration.
         *
         * The `$config` array follows the normalized native configuration schema.
         * The optional `consumer.wait_timeout` key (integer milliseconds, default
         * 30000, bounded 1000..86400000) caps how long `consumer()` blocks while
         * a broker connection becomes ready before failing with a
         * ConnectionException.
         *
         * @param array $config
         */
        public function __construct(array $config) {}

        /**
         * Auto-flushes buffered messages when the pool is garbage collected.
         */
        public function __destruct() {}

        /**
         * Purges all messages from a queue on the given broker.
         *
         * Flushes the publish buffer first (quiescing outstanding pipelined
         * drains) so buffered publications cannot repopulate the queue after
         * the purge.
         *
         * @param string $broker
         * @param string $queue
         * @return void
         */
        public function clear(string $broker, string $queue): void {}

        /**
         * Removes every registered event callback, returning how many were
         * removed (connection-state and backpressure combined).
         *
         * Connections sharing one native pool each register their own callbacks;
         * clearing allows a fresh registration to start from a clean slate.
         *
         * @return int
         */
        public function clearEventCallbacks(): int {}

        /**
         * Closes this pool handle.
         *
         * @return void
         */
        public function close(): void {}

        /**
         * Opens a consumer for a configured profile.
         *
         * @param string $profile
         * @return \Goopil\RabbitRs\Consumer
         */
        public function consumer(string $profile): \Goopil\RabbitRs\Consumer {}

        /**
         * Drains non-confirmed publish outcomes recorded by the pipelined
         * flush, returning one hash per record with `kind`, `message_id`, and
         * `message`. The queue is cleared by this call; the same records would
         * otherwise surface as exceptions at the next publish/flush/size/
         * clear/stats operation.
         *
         * @return list<array{kind: string, message_id: string, message: string}>
         *
         * @return array
         */
        public function drainErrors(): array {}

        /**
         * Flushes the publish buffer, sending all buffered messages to the broker.
         *
         * Outstanding pipelined drains are quiesced first (bounded by the fixed
         * teardown budget) so their re-buffered publications are visible to this
         * drain. The flush itself keeps full-deadline semantics: every buffered
         * publication is confirmed — or its failure raised — when `flush`
         * returns.
         *
         * In blind mode this is a barrier: every request enqueued on the publish
         * pump before this call — including buffered publications flushed just
         * above and any earlier blind publish — has been handed to the transport
         * (or dropped for lack of a channel during recovery) when `flush`
         * returns. Hand-off is not delivery: per the blind fire-and-forget
         * contract, a later transport failure is a silent loss.
         *
         * @return void
         */
        public function flush(): void {}

        /**
         * Registers a PHP callback invoked when publisher backpressure is detected.
         *
         * The callback receives `(string $broker, int $inFlight, int $capacity)`.
         * It is invoked synchronously on the PHP thread during publish, consume,
         * and `stats()` operations.
         *
         * @param callable(string, int, int): void $callback
         *
         * @param mixed $callback
         * @return void
         */
        public function onBackpressure(mixed $callback): void {}

        /**
         * Registers a PHP callback invoked when a broker connection state changes.
         *
         * The callback receives `(string $broker, string $state, int $generation)`.
         * It is invoked synchronously on the PHP thread during publish, consume,
         * and `stats()` operations.
         *
         * @param callable(string, string, int): void $callback
         *
         * @param mixed $callback
         * @return void
         */
        public function onConnectionState(mixed $callback): void {}

        /**
         * Publishes one message and returns its stable message identifier.
         *
         * @param array{broker: string, exchange: string, routing_key: string,
         *   payload: string, message_id: string, content_type?: string,
         *   correlation_id?: string, delay_ms?: int, timeout_ms?: int,
         *   headers?: array<string, bool|int|float|string|null>} $message
         *
         * Payload and all headers are limited to 1 MiB and 64 KiB per call
         * respectively. Headers are flat, contain at most 128 entries, and
         * `timeout_ms` is between 1 and 86,400,000.
         *
         * @throws \Goopil\RabbitRs\BackpressureException when the bounded publish
         *   buffer is full (outage with sustained traffic); retry with the same
         *   message later. Already-buffered messages are never dropped.
         *
         * @param array $message
         * @return string
         */
        public function publish(array $message): string {}

        /**
         * Publishes multiple messages in one boundary crossing.
         *
         * @param list<array{broker: string, exchange: string, routing_key: string,
         *   payload: string, message_id: string, content_type?: string,
         *   correlation_id?: string, delay_ms?: int, timeout_ms?: int,
         *   headers?: array<string, bool|int|float|string|null>}> $messages
         * @return list<string>
         *
         * A batch contains at most 256 messages and 1 MiB of cumulative payload.
         * Header count and size limits are cumulative across the complete call.
         *
         * @param array $messages
         * @return array
         */
        public function publishBatch(array $messages): array {}

        /**
         * Returns the number of pending messages in a queue on the given broker.
         *
         * Flushes the publish buffer first (quiescing outstanding pipelined
         * drains) so publications accepted by this pool are counted.
         *
         * @param string $broker
         * @param string $queue
         * @return int
         */
        public function size(string $broker, string $queue): int {}

        /**
         * Returns the current native metrics snapshot.
         *
         * A pending pipelined publish failure surfaces here (thrown) before the
         * snapshot is built, so a failed publish is observable at the next
         * stats operation at the latest.
         *
         * @return array{closed: bool, pid: int, handle: string,
         *   publishes_total: int, confirmations_total: int, returns_total: int,
         *   backpressure_total: int, reconnects_total: int, deliveries_total: int,
         *   duplicates_total: int, acks_total: int, rejects_total: int,
         *   dropped_publications_total: int, dropped_error_records_total: int,
         *   publish_buffered: int, publish_buffered_bytes: int,
         *   confirmation_latency_p50: int, confirmation_latency_p95: int,
         *   confirmation_latency_p99: int, settlement_latency_p50: int,
         *   settlement_latency_p95: int, settlement_latency_p99: int}
         *
         * Latency percentiles are integer milliseconds (`0` when no samples have
         * been recorded yet).
         *
         * @return array
         */
        public function stats(): array {}
    }
}
