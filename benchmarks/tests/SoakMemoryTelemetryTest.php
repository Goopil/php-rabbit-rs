<?php

declare(strict_types=1);

require_once __DIR__.'/../driver-bench/bin/soak_memory.php';

describe('soak memory telemetry (Round K #143)', function () {
    it('reads the process RSS as positive bytes', function () {
        $rss = rssSampleBytes();
        expect($rss)->toBeInt();
        expect($rss)->toBeGreaterThan(0);
    });

    it('fits a flat RSS series to a zero slope', function () {
        $mib = 1024 * 1024;
        $samples = [
            ['t_s' => 0.0, 'rss_bytes' => 100 * $mib],
            ['t_s' => 600.0, 'rss_bytes' => 100 * $mib],
            ['t_s' => 1200.0, 'rss_bytes' => 100 * $mib],
            ['t_s' => 1800.0, 'rss_bytes' => 100 * $mib],
        ];

        expect(rssSlopeMbPerHour($samples, warmupS: 360.0))->toBe(0.0);
    });

    it('detects a linear 50 MB/h leak', function () {
        $mib = 1024 * 1024;
        $start = 100.0 * $mib;
        $perHour = 50.0 * $mib;
        $samples = [
            ['t_s' => 0.0, 'rss_bytes' => (int) $start],
            ['t_s' => 600.0, 'rss_bytes' => (int) ($start + $perHour * 600.0 / 3600.0)],
            ['t_s' => 1200.0, 'rss_bytes' => (int) ($start + $perHour * 1200.0 / 3600.0)],
            ['t_s' => 1800.0, 'rss_bytes' => (int) ($start + $perHour * 1800.0 / 3600.0)],
        ];

        $slope = rssSlopeMbPerHour($samples, warmupS: 300.0);
        expect($slope)->toBeGreaterThanOrEqual(49.0);
        expect($slope)->toBeLessThanOrEqual(51.0);
    });

    it('excludes warmup samples from the fit', function () {
        $mib = 1024 * 1024;
        // Huge pre-warmup ramp (allocator warm-up): must not influence the fit.
        $samples = [
            ['t_s' => 0.0, 'rss_bytes' => 10 * $mib],
            ['t_s' => 300.0, 'rss_bytes' => 300 * $mib],
            ['t_s' => 900.0, 'rss_bytes' => 300 * $mib],
            ['t_s' => 1800.0, 'rss_bytes' => 300 * $mib],
        ];

        expect(rssSlopeMbPerHour($samples, warmupS: 600.0))->toBe(0.0);
    });

    it('returns null when no fit is possible', function () {
        // No samples at all.
        expect(rssSlopeMbPerHour([], warmupS: 0.0))->toBeNull();
        // Fewer than two post-warmup samples.
        expect(rssSlopeMbPerHour([['t_s' => 0.0, 'rss_bytes' => 1024]], warmupS: 0.0))->toBeNull();
        // Everything inside the warmup window.
        expect(rssSlopeMbPerHour([
            ['t_s' => 0.0, 'rss_bytes' => 1024],
            ['t_s' => 10.0, 'rss_bytes' => 2048],
        ], warmupS: 600.0))->toBeNull();
    });
});
