<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Connectors;

use Closure;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Illuminate\Queue\Connectors\ConnectorInterface;
use Illuminate\Support\Facades\Log;
use InvalidArgumentException;

final class RabbitMqConnector implements ConnectorInterface
{
    /** Whether the poison-friendly defaults warning was already emitted in this process. */
    private static bool $unboundedRedeliveryWarningEmitted = false;

    private readonly WorkerProfileResolver $workerProfiles;
    /**
     * @param array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array{safety: string, confirms: bool, mandatory: bool, confirm_timeout: int},
     *     topology: array<string, mixed>
     * } $normalizedConfig
     * @param (Closure(): bool)|null $inProductionEnvironment
     */
    public function __construct(
        private readonly NativePoolFactory $pools,
        private readonly array $normalizedConfig,
        private readonly ?Closure $inProductionEnvironment = null,
        private readonly bool $productionWarningEnabled = true,
    ) {
        $this->workerProfiles = new WorkerProfileResolver(
            $this->normalizedConfig['native']['workers'] ?? [],
        );
    }

    /**
     * @param array<string, mixed> $config
     */
    public function connect(array $config): RabbitMqQueue
    {
        $this->warnOnUnboundedRedeliveryDefaults($config);

        $defaultQueue = $config['queue'] ?? 'default';
        if (! is_string($defaultQueue) || $defaultQueue === '') {
            throw new InvalidArgumentException('queue must be a non-empty string');
        }
        $dispatchAfterCommit = $config['after_commit'] ?? false;
        if (! is_bool($dispatchAfterCommit)) {
            throw new InvalidArgumentException('after_commit must be a boolean');
        }
        $blockFor = $config['block_for'] ?? null;
        if ($blockFor !== null && (! is_int($blockFor) || $blockFor < 0)) {
            throw new InvalidArgumentException('block_for must be a non-negative integer or null');
        }
        if ($blockFor !== null && $blockFor > intdiv(PHP_INT_MAX, 1000)) {
            throw new InvalidArgumentException('block_for exceeds the supported millisecond range');
        }

        $autoSubscribe = $config['auto_subscribe']
            ?? ($this->normalizedConfig['auto_subscribe'] ?? false);
        if (! is_bool($autoSubscribe)) {
            throw new InvalidArgumentException('auto_subscribe must be a boolean');
        }

        $worker = $config['worker'] ?? 'default';
        $class = $worker === 'horizon'
            ? HorizonRabbitMqQueue::class
            : RabbitMqQueue::class;

        return new $class(
            $this->pools->make($this->normalizedConfig['native']),
            $this->normalizedConfig['routes'],
            $defaultQueue,
            $dispatchAfterCommit,
            workerProfiles: $this->workerProfiles,
            blockForMilliseconds: ($blockFor ?? 0) * 1000,
            publisherConfig: $this->normalizedConfig['publisher'],
            autoSubscribe: $autoSubscribe,
            hasDeadLetter: ($this->normalizedConfig['topology']['dead_letter'] ?? null) !== null,
        );
    }

    /**
     * Warns once per process when a connection resolves in production while
     * delivery_limit and dead_letter are both unset: a poison message (worker
     * crash before settlement) is then redelivered forever. The warning is
     * silenced with production_warning => false on the connection or in the
     * package config.
     *
     * @param array<string, mixed> $config
     */
    private function warnOnUnboundedRedeliveryDefaults(array $config): void
    {
        if (self::$unboundedRedeliveryWarningEmitted) {
            return;
        }

        if ($this->inProductionEnvironment === null || ! ($this->inProductionEnvironment)()) {
            return;
        }

        if (($this->normalizedConfig['topology']['queue']['delivery_limit'] ?? null) !== null) {
            return;
        }

        if (($this->normalizedConfig['topology']['dead_letter'] ?? null) !== null) {
            return;
        }

        if (! (bool) ($config['production_warning'] ?? $this->productionWarningEnabled)) {
            return;
        }

        self::$unboundedRedeliveryWarningEmitted = true;

        Log::warning(
            'rabbit-rs: delivery_limit and dead_letter are both unset for this connection. '
            .'A poison message (worker crash before settlement) will be redelivered forever. '
            .'Set topology.queue.delivery_limit with topology.dead_letter, or silence this '
            .'with production_warning => false.'
        );
    }
}
