<?php

declare(strict_types=1);

/*
 * Round K #143 — soak memory telemetry helpers.
 *
 * Kept in a standalone include so the benchmark metric-contract suite can
 * unit-test the RSS reader and the leak detector without running the soak.
 */

/**
 * Current process RSS in bytes.
 *
 * Linux: /proc/self/status VmRSS (kB). macOS: `ps -o rss= -p <pid>` (KB) —
 * the pattern already used by the suite per docs/performance.md.
 *
 * Returns null when the platform provides no readable RSS source.
 */
function rssSampleBytes(): ?int
{
    if (PHP_OS_FAMILY === 'Linux' && is_readable('/proc/self/status')) {
        $status = (string) file_get_contents('/proc/self/status');
        if (preg_match('/^VmRSS:\s+(\d+)\s+kB/m', $status, $m) === 1) {
            return (int) $m[1] * 1024;
        }

        return null;
    }

    if (PHP_OS_FAMILY === 'Darwin') {
        exec(sprintf('ps -o rss= -p %d 2>/dev/null', (int) getmypid()), $output, $rc);
        if ($rc === 0 && isset($output[0]) && trim($output[0]) !== '') {
            return ((int) trim($output[0])) * 1024;
        }
    }

    return null;
}

/**
 * Least-squares RSS slope over post-warmup samples, in MB per hour.
 *
 * Samples with t_s before the warmup boundary are excluded (allocator
 * steady state, topology declarations, runtime warm). Returns null when
 * fewer than two post-warmup samples exist — no verdict, not a pass.
 *
 * @param list<array{t_s: float|int, rss_bytes: int}> $samples
 */
function rssSlopeMbPerHour(array $samples, float $warmupS): ?float
{
    $n = 0;
    $sumT = 0.0;
    $sumR = 0.0;
    foreach ($samples as $sample) {
        $t = (float) $sample['t_s'];
        if ($t < $warmupS) {
            continue;
        }
        $n++;
        $sumT += $t;
        $sumR += (float) $sample['rss_bytes'];
    }

    if ($n < 2) {
        return null;
    }

    $meanT = $sumT / $n;
    $meanR = $sumR / $n;

    $num = 0.0;
    $den = 0.0;
    foreach ($samples as $sample) {
        $t = (float) $sample['t_s'];
        if ($t < $warmupS) {
            continue;
        }
        $num += ($t - $meanT) * ((float) $sample['rss_bytes'] - $meanR);
        $den += ($t - $meanT) ** 2;
    }

    if ($den === 0.0) {
        // Every post-warmup sample shares one timestamp: no time signal.
        return null;
    }

    return ($num / $den) * 3600.0 / (1024.0 * 1024.0);
}
