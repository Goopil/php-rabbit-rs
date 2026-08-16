<?php

declare(strict_types=1);

namespace Drivers;

interface BenchmarkDriver
{
    public function setup(): void;

    public function publish(array $messages): void;

    public function consume(int $count): void;

    public function reset(): void;

    /**
     * @return array{
     *     throughput: float,
     *     p50: float,
     *     p95: float,
     *     p99: float,
     *     cpu_seconds: float,
     *     rss_kb: int,
     *     connections: int,
     *     channels: int,
     *     duplicates: int,
     *     losses: int
     * }
     */
    public function metrics(): array;
}
