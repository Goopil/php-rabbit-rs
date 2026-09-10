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
    /**
     * Readiness wait for the declare probe, in milliseconds. The compiled
     * config emits consumer.wait_timeout in milliseconds (see
     * ConnectionCompiler::consumer()), mirroring the core's ms deserialization.
     */
    public const DECLARE_READINESS_TIMEOUT_MS = 2_000;

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
     * @param  array<string, mixed>  $nativeConfig
     */
    public function broker(array $nativeConfig): ?string
    {
        return $this->probePool($nativeConfig, function (Pool $pool) use ($nativeConfig): void {
            $pool->size(self::brokerName($nativeConfig), self::queueName($nativeConfig));
        });
    }

    /**
     * Checks queue existence with a passive probe (`Pool::size()`), returning
     * the native error message when the queue is missing (AMQP NOT-FOUND) or
     * the broker cannot be reached, and null when it exists.
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    public function queueSize(array $nativeConfig, string $broker, string $queue): ?string
    {
        return $this->probePool($nativeConfig, function (Pool $pool) use ($broker, $queue): void {
            $pool->size($broker, $queue);
        });
    }

    /**
     * Declares the worker profile's topology by opening and closing a
     * transient consumer: the native connection brings up connection,
     * channels, exchanges, queues, bindings and consumers in recovery order,
     * which in declare mode creates anything missing. Returns the error
     * message, or null when the declaration succeeded.
     *
     * The probe runs against {@see declareConfig()}, which bounds the
     * consumer readiness wait to {@see DECLARE_READINESS_TIMEOUT_MS}: the
     * declaration lands before consumer readiness in the native bring-up
     * order, and the topology command's warn path covers the
     * readiness-unconfirmed case. Waiting for the full compiled timeout
     * (30 s by default) would stall `rabbit-rs:topology --fix` whenever no
     * worker consumes (lab report bug 11 remainder).
     *
     * @param  array<string, mixed>  $nativeConfig
     */
    public function declareTopology(array $nativeConfig, string $workerProfile): ?string
    {
        return $this->probePool(self::declareConfig($nativeConfig), function (Pool $pool) use ($workerProfile): void {
            $consumer = $pool->consumer($workerProfile);
            $consumer->close();
        });
    }

    /**
     * @internal exposed for tests; use declareTopology() instead.
     *
     * Returns a copy of the native config whose consumer readiness wait is
     * bounded to {@see DECLARE_READINESS_TIMEOUT_MS}, leaving every other
     * key untouched. Hand-written probe configs may omit the consumer
     * section (or carry a non-array value), in which case a minimal one is
     * created on the copy.
     *
     * @param  array<string, mixed>  $nativeConfig
     * @return array<string, mixed>
     */
    public static function declareConfig(array $nativeConfig): array
    {
        $probed = $nativeConfig;
        $consumer = $probed['consumer'] ?? null;
        if (! is_array($consumer)) {
            $consumer = [];
        }

        $consumer['wait_timeout'] = self::DECLARE_READINESS_TIMEOUT_MS;
        $probed['consumer'] = $consumer;

        return $probed;
    }

    /**
     * Runs one probe against a transient pool and reports the native error
     * message, or null when the probe succeeded. The pool is always closed.
     *
     * @param  array<string, mixed>  $nativeConfig
     * @param  callable(Pool): void  $probe
     */
    private function probePool(array $nativeConfig, callable $probe): ?string
    {
        try {
            $pool = new Pool($nativeConfig);
            try {
                $probe($pool);
            } finally {
                $pool->close();
            }
        } catch (\Throwable $e) {
            return $e->getMessage();
        }

        return null;
    }

    /**
     * @param  array<string, mixed>  $nativeConfig
     */
    private static function brokerName(array $nativeConfig): string
    {
        return $nativeConfig['brokers'][0]['name'] ?? 'default';
    }

    /**
     * @param  array<string, mixed>  $nativeConfig
     */
    private static function queueName(array $nativeConfig): string
    {
        return $nativeConfig['workers'][0]['subscriptions'][0]['queue'] ?? 'default';
    }
}
