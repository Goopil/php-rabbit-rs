<?php

// Stubs for rabbit-rs

namespace Goopil\RabbitRs {
    class PhpClient {
        public function getId(): string {}

        public function connect(): bool {}

        public function reconnect(): bool {}

        public function close(): bool {}

        public function forceClose(): bool {}

        public function ping(): bool {}

        public function openChannel(): \Goopil\RabbitRs\PhpChannel {}

        public function __construct(?iterable $params) {}
    }

    class PhpChannel {
        public function getId(): int {}

        public function qos(int $prefetch_count, ?iterable $options): bool {}

        public function queueDeclare(string $name, ?iterable $options): bool {}

        public function queueBind(string $queue, string $exchange, ?string $routing_key, ?iterable $options): bool {}

        public function queueUnbind(string $queue, string $exchange, ?string $routing_key, ?iterable $options): bool {}

        public function queuePurge(string $name, ?iterable $options): bool {}

        public function queueDelete(string $name, ?iterable $options): bool {}

        public function exchangeDeclare(string $name, string $exchange_type, ?iterable $options): bool {}

        public function exchangeDelete(string $name, ?iterable $options): bool {}

        /**
         * Bind destination exchange to source exchange with an optional routing key.
         * Options: nowait (bool), arguments (FieldTable)
         */
        public function exchangeBind(string $destination, string $source, ?string $routing_key, ?iterable $options): bool {}

        /**
         * Unbind destination exchange from source exchange.
         * Options: nowait (bool), arguments (FieldTable)
         */
        public function exchangeUnbind(string $destination, string $source, ?string $routing_key, ?iterable $options): bool {}

        /**
         * Fetch one message synchronously from a queue (basic.get).
         * Options:
         * - no_ack: bool (default true). When false, the returned delivery must be acked/nacked.
         * Returns: AmqpDelivery or null if the queue is empty.
         */
        public function basicGet(string $queue, ?iterable $options): ? \Goopil\RabbitRs\AmqpDelivery {}

        public function basicPublish(string $exchange, string $routing_key, \Goopil\RabbitRs\AmqpMessage $message, ?iterable $options): bool {}

        /**
         * Put this channel into Publisher Confirms mode (php-amqplib compatible API).
         * The `nowait` parameter is accepted for API compatibility but ignored; enabling is lazy.
         */
        public function confirmSelect(?bool $nowait): bool {}

        /**
         * Wait until all pending publishes at the moment of the call are confirmed.
         * Returns true on success; throws on internal errors.
         */
        public function waitForConfirms(): bool {}

        /**
         * Wait until all pending publishes are confirmed, or throw if timeout / NACK occurs.
         * Argument is a timeout in milliseconds (optional). If null, waits without timeout.
         */
        public function waitForConfirmsOrDie(?int $timeout_ms): bool {}

        public function close(): bool {}

        /**
         * Start a simple consumer on a queue.
         * Options:
         * - no_ack: bool (default true) -> auto-ack mode when true (messages acknowledged automatically)
         *   When reject_on_exception=true and no_ack is not provided, no_ack is forced to false to allow NACKs (amqplib-compatible).
         * - reject_on_exception: bool (default false) -> when true, NACK on callback exception
         * - requeue_on_reject: bool (default false) -> controls NACK requeue behavior
         * - auto_qos: { enabled, min, max, step_up, step_down, cooldown_ms, target }
         */
        public function simpleConsume(string $queue, mixed $callback, ?iterable $options): string {}

        public function basicAck(int $delivery_tag, ?bool $multiple): bool {}

        public function basicNack(int $delivery_tag, bool $requeue, ?bool $multiple): bool {}

        public function basicReject(int $delivery_tag, bool $requeue): bool {}

        public function basicCancel(?string $tag): bool {}

        public function wait(int $timeout_ms, int $max): int {}

        public function __construct() {}
    }

    class AmqpMessage {
        public function __construct(mixed $body, ?iterable $properties) {}
    }

    class AmqpDelivery {
        public function getBody(): string {}

        public function getExchange(): string {}

        public function getRoutingKey(): string {}

        public function isRedelivered(): bool {}

        public function getDeliveryTag(): int {}

        public function getContentType(): ?string {}

        public function getContentEncoding(): ?string {}

        public function getCorrelationId(): ?string {}

        public function getReplyTo(): ?string {}

        public function getExpiration(): ?string {}

        public function getMessageId(): ?string {}

        public function getTimestamp(): ?int {}

        public function getType(): ?string {}

        public function getUserId(): ?string {}

        public function getAppId(): ?string {}

        public function getClusterId(): ?string {}

        public function getPriority(): ?int {}

        public function getDeliveryMode(): ?int {}

        public function getHeaders(): array {}

        public function ack(): bool {}

        public function nack(bool $requeue): bool {}

        public function reject(bool $requeue): bool {}

        public function __construct() {}
    }
}
