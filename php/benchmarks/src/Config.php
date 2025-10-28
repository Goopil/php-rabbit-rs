<?php

namespace Goopil\RabbitRs\Benchmarks;

class Config
{
    public const RABBITMQ_HOST = '127.0.0.1';
    public const RABBITMQ_PORT = 5672;
    public const RABBITMQ_USER = 'guest';
    public const RABBITMQ_PASSWORD = 'guest';
    public const RABBITMQ_VHOST = '/';

    // Benchmark parameters
    public const MESSAGE_COUNT = 10000;
    public const CONCURRENT_CONNECTIONS = 10;
    public const WARMUP_ROUNDS = 3;
    public const BENCHMARK_ROUNDS = 10;

    // Queue/exchange settings
    public const EXCHANGE_NAME = 'benchmark_exchange';
    public const QUEUE_NAME = 'benchmark_queue';
    public const ROUTING_KEY = 'benchmark.key';
}
