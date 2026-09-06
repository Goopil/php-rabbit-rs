<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Console;

use Goopil\RabbitRs\Pool;

/**
 * Environment probes for rabbit-rs:doctor. Resolved from the container so
 * test suites running without ext-rabbit_rs (and without a broker) can bind
 * a fake: the real broker probe performs an AMQP connection.
 */
class DoctorProbe
{
    public function extensionLoaded(): bool
    {
        return extension_loaded('rabbit_rs');
    }

    public function extensionVersion(): ?string
    {
        $version = phpversion('rabbit_rs');

        return $version === false ? null : $version;
    }

    /**
     * Connects to the broker described by the compiled native config and
     * returns the error message, or null when the connection succeeded
     * (reachability, auth, and vhost are validated by the native client).
     *
     * The pool construction is lazy, so size() is the call that actually
     * touches the broker; the probe pool is closed immediately afterwards.
     *
     * @param array<string, mixed> $nativeConfig
     */
    public function broker(array $nativeConfig): ?string
    {
        try {
            $pool = new Pool($nativeConfig);
            try {
                $pool->size(self::brokerName($nativeConfig), self::queueName($nativeConfig));
            } finally {
                $pool->close();
            }
        } catch (\Throwable $e) {
            return $e->getMessage();
        }

        return null;
    }

    /**
     * Checks queue existence with a passive probe (`Pool::size()`), returning
     * the native error message when the queue is missing (AMQP NOT-FOUND) or
     * the broker cannot be reached, and null when it exists.
     *
     * @param array<string, mixed> $nativeConfig
     */
    public function queueSize(array $nativeConfig, string $broker, string $queue): ?string
    {
        try {
            $pool = new Pool($nativeConfig);
            try {
                $pool->size($broker, $queue);
            } finally {
                $pool->close();
            }
        } catch (\Throwable $e) {
            return $e->getMessage();
        }

        return null;
    }

    /**
     * Declares the worker profile's topology by opening and closing a
     * transient consumer: the native connection brings up connection,
     * channels, exchanges, queues, bindings and consumers in recovery order,
     * which in declare mode creates anything missing. Returns the error
     * message, or null when the declaration succeeded.
     *
     * @param array<string, mixed> $nativeConfig
     */
    public function declareTopology(array $nativeConfig, string $workerProfile): ?string
    {
        try {
            $pool = new Pool($nativeConfig);
            try {
                $consumer = $pool->consumer($workerProfile);
                $consumer->close();
            } finally {
                $pool->close();
            }
        } catch (\Throwable $e) {
            return $e->getMessage();
        }

        return null;
    }

    /**
     * @param array<string, mixed> $nativeConfig
     */
    private static function brokerName(array $nativeConfig): string
    {
        return $nativeConfig['brokers'][0]['name'] ?? 'default';
    }

    /**
     * @param array<string, mixed> $nativeConfig
     */
    private static function queueName(array $nativeConfig): string
    {
        return $nativeConfig['workers'][0]['subscriptions'][0]['queue'] ?? 'default';
    }
}
