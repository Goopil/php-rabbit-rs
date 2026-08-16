<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Support;

use Illuminate\Support\Str;
use InvalidArgumentException;

final class MessageMapper
{
    /**
     * @param array{confirm_timeout?: int} $publisherConfig
     */
    public function __construct(private readonly array $publisherConfig = [])
    {
    }

    /**
     * @param array{broker: string, exchange: string, routing_key: string} $route
     * @param array<string, mixed> $options
     * @return array<string, mixed>
     */
    public function map(
        string $payload,
        array $route,
        string $queue,
        array $options = [],
        ?int $delayMilliseconds = null,
    ): array {
        $message = [
            'broker' => $route['broker'],
            'exchange' => $route['exchange'],
            'routing_key' => str_replace('{queue}', $queue, $route['routing_key']),
            'payload' => $payload,
            'message_id' => $this->messageId($payload, $options['message_id'] ?? null),
        ];

        foreach (['content_type', 'correlation_id', 'headers'] as $option) {
            if (array_key_exists($option, $options)) {
                $message[$option] = $options[$option];
            }
        }

        if (array_key_exists('timeout_ms', $options)) {
            $message['timeout_ms'] = $options['timeout_ms'];
        } elseif (isset($this->publisherConfig['confirm_timeout'])) {
            $message['timeout_ms'] = $this->publisherConfig['confirm_timeout'];
        }

        if ($delayMilliseconds !== null) {
            $message['delay_ms'] = $delayMilliseconds;
        }

        return $message;
    }

    private function messageId(string $payload, mixed $configured): string
    {
        if ($configured !== null) {
            if (! is_string($configured) || ! Str::isUuid($configured)) {
                throw new InvalidArgumentException('options.message_id must be a valid UUID');
            }

            return $configured;
        }

        $decoded = json_decode($payload, true);
        $payloadUuid = is_array($decoded) ? ($decoded['uuid'] ?? null) : null;

        return is_string($payloadUuid) && Str::isUuid($payloadUuid)
            ? $payloadUuid
            : (string) Str::uuid();
    }
}