<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Connectors;

use Closure;
use Goopil\RabbitRs\Laravel\Config\ConnectionCompiler;
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

    /**
     * @param array<string, mixed> $defaults package defaults (config('rabbit-rs')) merged under every connection config
     * @param (Closure(): bool)|null $inProductionEnvironment
     */
    public function __construct(
        private readonly NativePoolFactory $pools,
        private readonly array $defaults = [],
        private readonly ?Closure $inProductionEnvironment = null,
        private readonly bool $productionWarningEnabled = true,
    ) {}

    /**
     * Compiles the queue connection lazily: one connection = one broker = one
     * native pool. Framework keys (queue, after_commit, block_for, worker,
     * production_warning) stay read from the raw connection config.
     *
     * @param array<string, mixed> $config
     */
    public function connect(array $config): RabbitMqQueue
    {
        $name = $this->connectionName($config);
        $compiled = ConnectionCompiler::compile($name, $config, $this->defaults);

        $this->warnOnUnboundedRedeliveryDefaults($config, $compiled);

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

        $worker = $config['worker'] ?? 'default';
        $class = $worker === 'horizon'
            ? HorizonRabbitMqQueue::class
            : RabbitMqQueue::class;

        return new $class(
            $this->pools->make($compiled['native']),
            $compiled['routes'],
            $defaultQueue,
            $dispatchAfterCommit,
            workerProfiles: new WorkerProfileResolver($compiled['native']['workers']),
            blockForMilliseconds: ($blockFor ?? 0) * 1000,
            publisherConfig: $compiled['publisher'],
            autoSubscribe: $compiled['auto_subscribe'],
            hasDeadLetter: $compiled['topology']['dead_letter'] !== null,
        );
    }

    /**
     * Resolves the connection name by reverse lookup in queue.connections so
     * compiled brokers and worker profiles are named after the connection.
     * When two connections hold identical arrays, the first-found name wins —
     * identical configs compile identically, so the ambiguity is harmless.
     * Falls back to 'default' when the config is not registered (direct
     * connector use).
     *
     * @param array<string, mixed> $config
     */
    private function connectionName(array $config): string
    {
        $connections = config('queue.connections');
        $found = is_array($connections) ? array_search($config, $connections, true) : false;

        return is_string($found) && $found !== '' ? $found : 'default';
    }

    /**
     * Warns once per process when a connection resolves in production while
     * delivery_limit and dead_letter are both unset: a poison message (worker
     * crash before settlement) is then redelivered forever. The warning is
     * silenced with production_warning => false on the connection or in the
     * package config.
     *
     * @param array<string, mixed> $config
     * @param array<string, mixed> $compiled
     */
    private function warnOnUnboundedRedeliveryDefaults(array $config, array $compiled): void
    {
        if (self::$unboundedRedeliveryWarningEmitted) {
            return;
        }

        if ($this->inProductionEnvironment === null || ! ($this->inProductionEnvironment)()) {
            return;
        }

        if (($compiled['topology']['queue']['delivery_limit'] ?? null) !== null) {
            return;
        }

        if (($compiled['topology']['dead_letter'] ?? null) !== null) {
            return;
        }

        if (! (bool) ($config['production_warning'] ?? $this->productionWarningEnabled)) {
            return;
        }

        self::$unboundedRedeliveryWarningEmitted = true;

        Log::warning(
            'rabbit-rs: delivery_limit and dead_letter are both unset for this connection. '
            .'A poison message (worker crash before settlement) will be redelivered forever. '
            .'Set delivery_limit with dead_letter on the queue connection, or silence this '
            .'with production_warning => false.'
        );
    }
}
