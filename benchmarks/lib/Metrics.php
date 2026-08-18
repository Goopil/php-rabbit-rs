<?php

declare(strict_types=1);

namespace Bench;

trait Metrics
{
    /** @var list<float> */
    private array $latencies = [];
    private int $startedAt = 0;

    public function recordLatency(float $ms): void
    {
        $this->latencies[] = $ms;
    }

    public function resetLatencies(): void
    {
        $this->latencies = [];
    }

    public function startTimer(): void
    {
        $this->startedAt = hrtime(true);
    }

    public function elapsedSeconds(): float
    {
        return $this->startedAt > 0 ? $this->elapsedNanos() / 1_000_000_000 : 0.0;
    }

    public function elapsedNanos(): int
    {
        return $this->startedAt > 0 ? hrtime(true) - $this->startedAt : 0;
    }

    public function percentile(float $p): float
    {
        if ($this->latencies === []) {
            return 0.0;
        }

        $sorted = $this->latencies;
        sort($sorted);
        $count = count($sorted);
        $index = (int) floor(($p / 100) * $count);

        return $sorted[min($index, $count - 1)];
    }

    protected function rssKb(): int
    {
        if (PHP_OS_FAMILY === 'Darwin') {
            $output = @shell_exec('ps -o rss= -p ' . getmypid());
            if ($output !== null && $output !== '') {
                return (int) trim($output);
            }
        } elseif (PHP_OS_FAMILY === 'Linux') {
            $status = @file_get_contents('/proc/self/status');
            if ($status !== false && preg_match('/VmRSS:\s+(\d+)/', $status, $m)) {
                return (int) $m[1];
            }
        }

        return 0;
    }

    protected function cpuSeconds(): float
    {
        $usage = getrusage();
        if ($usage === false) {
            return 0.0;
        }

        $utime = ($usage['ru_utime.tv_sec'] ?? 0) + ($usage['ru_utime.tv_usec'] ?? 0) / 1_000_000;
        $stime = ($usage['ru_stime.tv_sec'] ?? 0) + ($usage['ru_stime.tv_usec'] ?? 0) / 1_000_000;

        return round($utime + $stime, 4);
    }

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
    protected function buildMetrics(
        int $count,
        float $elapsed,
        int $connections = 0,
        int $channels = 0,
        int $duplicates = 0,
        int $losses = 0,
    ): array {
        $throughput = $elapsed > 0 ? $count / $elapsed : 0.0;

        return [
            'throughput' => round($throughput, 2),
            'p50' => round($this->percentile(50), 3),
            'p95' => round($this->percentile(95), 3),
            'p99' => round($this->percentile(99), 3),
            'cpu_seconds' => $this->cpuSeconds(),
            'rss_kb' => $this->rssKb(),
            'connections' => $connections,
            'channels' => $channels,
            'duplicates' => $duplicates,
            'losses' => $losses,
        ];
    }
}
