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
 * Least-squares RSS slope over the peak-envelope of post-warmup samples,
 * in MB per hour.
 *
 * The envelope is the running max seeded from the first sample (the warmup
 * peak included). Rationale (kill-60min calibration run, 2026-09-05): under
 * kill churn the macOS allocator oscillates between two bounded states
 * (~25 MB and ~90 MB) with deep transient dips; a raw fit over the
 * oscillating series misreads uneven plateau durations as growth (+63 MB/h
 * on a demonstrably leak-free run). A genuine leak must exceed the warmup
 * peak and keep climbing — which raises the envelope — while bounded
 * oscillation keeps it flat.
 *
 * Samples with t_s before the warmup boundary are excluded from the fit
 * (allocator steady state, topology declarations, runtime warm). Returns
 * null when fewer than two post-warmup samples exist — no verdict, not a
 * pass.
 *
 * @param list<array{t_s: float|int, rss_bytes: int}> $samples
 */
function rssSlopeMbPerHour(array $samples, float $warmupS): ?float
{
    $n = 0;
    $sumT = 0.0;
    $sumR = 0.0;
    $peak = null;
    foreach ($samples as $sample) {
        $t = (float) $sample['t_s'];
        $r = (float) $sample['rss_bytes'];
        $peak = $peak === null ? $r : max($peak, $r);
        if ($t < $warmupS) {
            continue;
        }
        $n++;
        $sumT += $t;
        $sumR += $peak;
    }

    if ($n < 2) {
        return null;
    }

    $meanT = $sumT / $n;
    $meanR = $sumR / $n;

    $num = 0.0;
    $den = 0.0;
    $peak = null;
    foreach ($samples as $sample) {
        $t = (float) $sample['t_s'];
        $r = (float) $sample['rss_bytes'];
        $peak = $peak === null ? $r : max($peak, $r);
        if ($t < $warmupS) {
            continue;
        }
        $num += ($t - $meanT) * ($peak - $meanR);
        $den += ($t - $meanT) ** 2;
    }

    if ($den === 0.0) {
        // Every post-warmup sample shares one timestamp: no time signal.
        return null;
    }

    return ($num / $den) * 3600.0 / (1024.0 * 1024.0);
}

/**
 * Marks sequence number $seq (1-based) as received in a compact bitset.
 *
 * The soak tracks every distinct published sequence; a PHP map grows
 * linearly with the published count (OOM at the 128M default limit on a
 * 30-min steady run, and a false-positive RSS slope — the harness itself
 * looked like a leak). A bitset is O(maxSeq/8) instead: ~375 KB for
 * 3 M messages. Re-marking the same seq is idempotent.
 */
function bitMarkReceived(string &$bits, int $seq): void
{
    $byte = intdiv($seq - 1, 8);
    if ($byte >= strlen($bits)) {
        $bits .= str_repeat("\0", $byte - strlen($bits) + 1);
    }
    $bits[$byte] = chr(ord($bits[$byte]) | (1 << (($seq - 1) % 8)));
}

/**
 * Counts distinct marked bits (popcount over the whole bitset).
 */
function bitPopcount(string $bits): int
{
    static $nibble = [0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4];

    $total = 0;
    foreach (count_chars($bits) as $byte => $freq) {
        $total += $freq * ($nibble[$byte & 0x0F] + $nibble[$byte >> 4]);
    }

    return $total;
}
