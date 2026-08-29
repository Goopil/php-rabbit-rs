<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Connectors;

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Goopil\RabbitRs\Laravel\Support\WorkerProfileResolver;
use Illuminate\Queue\Connectors\ConnectorInterface;
use InvalidArgumentException;

final class RabbitMqConnector implements ConnectorInterface
{
    private readonly WorkerProfileResolver $workerProfiles;
    /**
     * @param array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array{confirms: bool, mandatory: bool, confirm_timeout: int},
     *     topology: array<string, mixed>
     * } $normalizedConfig
     */
    public function __construct(
        private readonly NativePoolFactory $pools,
        private readonly array $normalizedConfig,
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
            $this->pools->make($this->normalizedConfig['native']),
            $this->normalizedConfig['routes'],
            $defaultQueue,
            $dispatchAfterCommit,
            workerProfiles: $this->workerProfiles,
            blockForMilliseconds: ($blockFor ?? 0) * 1000,
            publisherConfig: $this->normalizedConfig['publisher'],
        );
    }
}
