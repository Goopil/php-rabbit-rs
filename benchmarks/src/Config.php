<?php

declare(strict_types=1);

namespace Bench;

// Benchmark Protocol:
// - Machine: recorded in output
// - PHP version: phpversion()
// - RabbitMQ version: recorded manually
// - Payload: Config::MESSAGE_PAYLOAD_BYTES bytes
// - Warm-up: one full publish+consume cycle before measured rounds
// - Iterations: Config::BENCHMARK_ROUNDS
// - Metrics: median, p99, dispersion (stdev)
// - Scenarios: reliable (confirms=true, manual ack),
//              reliable batch (confirms=true, batch ack),
//              best-effort (confirms=false and/or early_ack)
// - Verification: losses=0 AND duplicates=0 for reliable modes
/**
 * Benchmark configuration.
 *
 * @noinspection PhpUnnecessaryFullyQualifiedNameInspection
 *
 * Credentials are local lab-only values (rabbit_rs_lab). They are NOT production
 * secrets. SonarCloud S2068 is a false positive for this context.
 */
class Config
{
    public const RABBITMQ_HOST = '127.0.0.1';
    public const RABBITMQ_PORT = 5672;
    public const RABBITMQ_USER = 'rabbit_rs';
    public const RABBITMQ_PASSWORD = 'rabbit_rs_lab';
    public const RABBITMQ_VHOST = '/';

    public const MESSAGE_COUNT = 10000;
    public const BENCHMARK_ROUNDS = 10;
    public const MESSAGE_PAYLOAD_BYTES = 256;
    public const PREFETCH_COUNT = 128;
    public const MESSAGE_PAYLOAD_LARAVEL_BYTES = 1024;
    public const PREFETCH_LARAVEL = 64;

    public const EXCHANGE_NAME = 'benchmark_exchange';
    public const EXCHANGE_TYPE = 'direct';
    public const EXCHANGE_DURABLE = true;
    public const QUEUE_NAME = 'benchmark_queue';
    public const QUEUE_DURABLE = true;
    public const ROUTING_KEY = 'benchmark.key';
}
