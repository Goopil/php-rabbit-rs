<?php

declare(strict_types=1);

namespace Bench;

/**
 * Self-describing envelope for archived benchmark results (Round J metric
 * contract, #127): every published result set carries its broker/benchmark
 * configuration and runtime environment next to the measurements.
 *
 * Credentials are never emitted — the user is masked and the password is
 * omitted entirely.
 */
final class ResultMeta
{
    /**
     * Broker and benchmark configuration as run (credentials masked).
     *
     * @return array<string, mixed>
     */
    public static function config(int $payloadBytes = Config::MESSAGE_PAYLOAD_BYTES): array
    {
        return [
            'rabbitmq' => [
                'host' => Config::RABBITMQ_HOST,
                'port' => Config::RABBITMQ_PORT,
                'vhost' => Config::RABBITMQ_VHOST,
                'user' => '***',
            ],
            'message_count' => Config::MESSAGE_COUNT,
            'rounds' => Config::BENCHMARK_ROUNDS,
            'warmup_rounds' => 1,
            'payload_bytes' => $payloadBytes,
        ];
    }

    /**
     * Runtime environment of the process that produced the results.
     *
     * @return array<string, mixed>
     */
    public static function meta(): array
    {
        return [
            'php' => PHP_VERSION,
            'sapi' => PHP_SAPI,
            'os' => PHP_OS.' '.php_uname('r'),
            'extensions' => [
                'rabbit_rs' => phpversion('rabbit_rs') ?: false,
                'amqp' => phpversion('amqp') ?: false,
            ],
        ];
    }
}
