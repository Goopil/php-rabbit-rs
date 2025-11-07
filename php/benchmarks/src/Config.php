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
    public const MESSAGE_PAYLOAD_BYTES = 256;
    public const PREFETCH_COUNT = 500;
    public const CONSUMER_WAIT_TIMEOUT = 10; // seconds for blocking waits
    public const CONFIRM_WAIT_TIMEOUT = 60; // seconds max to await publisher confirms
    public const CONFIRM_CHUNK_SIZE = 1000;

    // Queue/exchange settings
    public const EXCHANGE_NAME = 'benchmark_exchange';
    public const EXCHANGE_TYPE = 'direct';
    public const EXCHANGE_DURABLE = true;
    public const QUEUE_NAME = 'benchmark_queue';
    public const QUEUE_DURABLE = true;
    public const ROUTING_KEY = 'benchmark.key';
}
