<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

use RuntimeException;

/**
 * Thrown when a delayed publish is attempted in `delay.mode=plugin` while the
 * broker proves the `rabbitmq_delayed_message_exchange` plugin absent: without
 * it every deferred message is silently lost, so the driver refuses loudly.
 */
final class DelayPluginMissingException extends RuntimeException
{
    public static function forConnection(string $connection): self
    {
        return new self(
            'rabbit-rs: delay.mode=plugin requires the rabbitmq_delayed_message_exchange plugin, '
            ."which is not enabled on broker '{$connection}' — enable the plugin on the broker, "
            ."or set queue.connections.{$connection}.delay.mode to auto or ttl.",
        );
    }
}
