<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Connectors;

use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;
use Illuminate\Queue\Connectors\ConnectorInterface;
use InvalidArgumentException;

final class RabbitMqConnector implements ConnectorInterface
{
    /**
     * @param array{
     *     native: array<string, mixed>,
     *     routes: array<string, array<string, mixed>>,
     *     publisher: array<string, bool>,
     *     topology: array<string, mixed>
     * } $normalizedConfig
     */
    public function __construct(
        private readonly NativePoolFactory $pools,
        private readonly array $normalizedConfig,
    ) {}

    /**
     * @param array<string, mixed> $config
     */
    public function connect(array $config): RabbitMqQueue
    {
        $defaultQueue = $config['queue'] ?? 'default';
        if (! is_string($defaultQueue) || $defaultQueue === '') {
            throw new InvalidArgumentException('queue must be a non-empty string');
        }

        return new RabbitMqQueue(
            $this->pools->make($this->normalizedConfig['native']),
            $this->normalizedConfig['routes'],
            $defaultQueue,
        );
    }
}