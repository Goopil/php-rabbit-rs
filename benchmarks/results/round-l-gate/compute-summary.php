<?php

/*
 * compute-summary.php — derive summary.json for the round-l-gate archive.
 *
 * Reads raw/<cell>-run{1..3}.json (gate runs only; probe files excluded),
 * computes per-cell statistics the same way the round-2/round-i archives
 * report them (round-median over 30 measured rounds = 3 interleaved runs
 * x 10 rounds; plus per-run aggregate medians, comparable to the
 * round-l-profile pass-level table), checks the untouchable invariants,
 * and compares against the frozen budget references.
 *
 * Verdict rule (fixed before the numbers were computed):
 *   INVALID  — any run breaks an invariant (ok=false, losses>0, late>0,
 *              duplicates>0, stall_recoveries>0, reconnects_total>0).
 *   REGRESSION (throughput) — cell round-median below its lowest frozen
 *              classic-queue median (round-i/round-d era floors), OR more
 *              than 25 % below the round-l-profile median (the same-round,
 *              same-machine like-for-like), OR the same-session ratio vs
 *              the unchanged vladimir control collapses below its frozen
 *              range (blind/vladimir-dispatch < 2.0x; worker/vladimir-worker
 *              < 6.0x).
 *   PASS     — everything else.
 */

$raw = __DIR__.'/raw';

$cells = [
    'goopil-dispatch-blind' => ['mode' => 'dispatch', 'driver' => 'rabbit-rs'],
    'goopil-dispatch-safe'  => ['mode' => 'dispatch', 'driver' => 'rabbit-rs'],
    'goopil-worker'         => ['mode' => 'worker', 'driver' => 'rabbit-rs'],
    'vladimir-dispatch'     => ['mode' => 'dispatch', 'driver' => 'rabbitmq-amqplib'],
    'vladimir-worker'       => ['mode' => 'worker', 'driver' => 'rabbitmq-amqplib'],
];

// Frozen references (round-median over 30 measured rounds, classic queues,
// driver-bench 1000x10 protocol) transcribed from the archive READMEs.
$frozen = [
    'round-2-rebench (2026-08-31)' => [
        'goopil-dispatch-blind' => 70262, 'goopil-dispatch-safe' => 7703,
        'goopil-worker' => 21747, 'vladimir-dispatch' => 32193, 'vladimir-worker' => 2041,
    ],
    'round-i-rebench (2026-09-03)' => [
        'goopil-dispatch-blind' => 21992, 'goopil-dispatch-safe' => 6534,
        'goopil-worker' => 16234, 'vladimir-dispatch' => 9685, 'vladimir-worker' => 2029,
    ],
    'round-d-safe-flush (2026-09-04, pipelined)' => [
        'goopil-dispatch-blind' => 21939, 'goopil-dispatch-safe' => 20866,
        'goopil-worker' => 15421, 'vladimir-dispatch' => 9545, 'vladimir-worker' => null,
    ],
    'round-l-profile (2026-10-04, same round, like-for-like)' => [
        'goopil-dispatch-blind' => 71380, 'goopil-dispatch-safe' => 65753,
        'goopil-worker' => 23495, 'vladimir-dispatch' => null, 'vladimir-worker' => null,
    ],
];

// Frozen same-session ratios (blind vs vladimir dispatch; worker vs vladimir worker).
$frozenRatios = [
    'blind/vladimir-dispatch' => ['round-i' => 2.3, 'round-2' => 2.2],
    'worker/vladimir-worker' => ['round-i' => 8.0, 'round-2' => 10.7],
];

$median = static function (array $a): ?float {
    sort($a);
    $n = count($a);
    if ($n === 0) {
        return null;
    }

    return $n % 2 ? (float) $a[intdiv($n, 2)] : ($a[$n / 2 - 1] + $a[$n / 2]) / 2;
};

$summary = ['generated' => date('c'), 'cells' => []];
$ratios = [];

foreach ($cells as $cell => $meta) {
    $runs = [];
    for ($i = 1; $i <= 3; $i++) {
        $path = "{$raw}/{$cell}-run{$i}.json";
        if (!is_file($path)) {
            fwrite(STDERR, "error: missing run JSON: {$path}\n");
            exit(1);
        }
        $runs[$i] = json_decode(file_get_contents($path), true);
    }

    $roundRates = [];
    $runAvgs = [];
    $invariants = ['ok' => true, 'losses' => 0, 'late' => 0, 'duplicates' => 0, 'stalls' => 0, 'reconnects' => 0];
    foreach ($runs as $run) {
        foreach ($run['rounds_detail'] as $r) {
            if (($r['rate_ops_s'] ?? 0) > 0) {
                $roundRates[] = $r['rate_ops_s'];
            }
            $invariants['stalls'] += (int) ($r['stall_recoveries'] ?? 0);
        }
        $runAvgs[] = (float) $run['avg_rate_ops_s'];
        $invariants['ok'] = $invariants['ok'] && (bool) $run['ok'];
        $invariants['losses'] += (int) $run['losses'];
        $invariants['late'] += (int) $run['late_arrivals_after_drain'];
        $invariants['duplicates'] += (int) ($run['duplicates'] ?? 0);
        if ($run['reconnects_total'] !== null) {
            $invariants['reconnects'] += (int) $run['reconnects_total'];
        }
    }

    $lat = static fn (string $key): ?float => $median(array_map(
        static fn (array $r): float => (float) ($r['latency_ms'][$key] ?? 0),
        array_values($runs),
    ));

    $roundMedian = $median($roundRates);
    $runMedian = $median($runAvgs);
    $cellSummary = [
        'mode' => $meta['mode'],
        'driver' => $meta['driver'],
        'rounds_measured' => count($roundRates),
        'round_median_ops_s' => $roundMedian !== null ? round($roundMedian, 0) : null,
        'round_min_ops_s' => $roundRates !== [] ? round(min($roundRates), 0) : null,
        'round_max_ops_s' => $roundRates !== [] ? round(max($roundRates), 0) : null,
        'run_avg_ops_s' => array_map(static fn ($v) => round((float) $v, 0), $runAvgs),
        'run_avg_median_ops_s' => $runMedian !== null ? round($runMedian, 0) : null,
        'latency_ms_p50' => $lat('p50'),
        'latency_ms_p95' => $lat('p95'),
        'latency_ms_p99' => $lat('p99'),
        'invariants' => $invariants,
        'invariants_ok' => $invariants['ok'] && $invariants['losses'] === 0 && $invariants['late'] === 0
            && ($meta['mode'] === 'dispatch' || $invariants['duplicates'] === 0)
            && $invariants['stalls'] === 0 && $invariants['reconnects'] === 0,
        'comparison_vs_frozen' => [],
    ];

    foreach ($frozen as $label => $refs) {
        $ref = $refs[$cell] ?? null;
        $cellSummary['comparison_vs_frozen'][$label] = $ref === null ? null : [
            'reference_ops_s' => $ref,
            'delta_pct' => round(($roundMedian - $ref) / $ref * 100, 1),
        ];
    }

    $summary['cells'][$cell] = $cellSummary;
}

// Same-session ratios (round-median based), the fair cross-session comparison.
$blind = $summary['cells']['goopil-dispatch-blind']['round_median_ops_s'];
$vd = $summary['cells']['vladimir-dispatch']['round_median_ops_s'];
$wk = $summary['cells']['goopil-worker']['round_median_ops_s'];
$vw = $summary['cells']['vladimir-worker']['round_median_ops_s'];
$summary['same_session_ratios'] = [
    'blind/vladimir-dispatch' => $vd > 0 ? round($blind / $vd, 2) : null,
    'worker/vladimir-worker' => $vw > 0 ? round($wk / $vw, 2) : null,
    'frozen_references' => $frozenRatios,
];

// Verdicts (rule fixed above, applied mechanically).
foreach ($summary['cells'] as $cell => &$c) {
    if (!$c['invariants_ok']) {
        $c['verdict'] = 'INVALID (invariants broken)';
        continue;
    }
    $regression = false;
    $why = [];
    // (b) lowest frozen classic-queue median floors (round-d/round-i era).
    $floors = ['goopil-dispatch-blind' => 21939, 'goopil-dispatch-safe' => 20866, 'goopil-worker' => 15421,
        'vladimir-dispatch' => 9545, 'vladimir-worker' => 2029];
    if ($c['round_median_ops_s'] < $floors[$cell]) {
        $regression = true;
        $why[] = "below frozen floor {$floors[$cell]}";
    }
    // (c) same-session ratio collapse.
    if ($cell === 'goopil-dispatch-blind' && $summary['same_session_ratios']['blind/vladimir-dispatch'] < 2.0) {
        $regression = true;
        $why[] = 'blind/vladimir-dispatch ratio < 2.0x';
    }
    if ($cell === 'goopil-worker' && $summary['same_session_ratios']['worker/vladimir-worker'] < 6.0) {
        $regression = true;
        $why[] = 'worker/vladimir-worker ratio < 6.0x';
    }
    // (d) > 25 % below the same-round profile comparator.
    $profile = $frozen['round-l-profile (2026-10-04, same round, like-for-like)'][$cell] ?? null;
    if ($profile !== null && $c['round_median_ops_s'] < $profile * 0.75) {
        $regression = true;
        $why[] = ">25 % below round-l-profile median {$profile}";
    }
    $c['verdict'] = $regression ? 'REGRESSION ('.implode('; ', $why).')' : 'PASS';
}
unset($c);

file_put_contents(__DIR__.'/summary.json', json_encode($summary, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n");

printf("%-22s %10s %10s %10s  %-6s %s\n", 'cell', 'round-med', 'run-med', 'frozen-floor', 'inv', 'verdict');
$floors = ['goopil-dispatch-blind' => 21939, 'goopil-dispatch-safe' => 20866, 'goopil-worker' => 15421,
    'vladimir-dispatch' => 9545, 'vladimir-worker' => 2029];
foreach ($summary['cells'] as $cell => $c) {
    printf("%-22s %10s %10s %10s  %-6s %s\n", $cell, number_format($c['round_median_ops_s']),
        number_format($c['run_avg_median_ops_s']), number_format($floors[$cell]),
        $c['invariants_ok'] ? 'ok' : 'BROKEN', $c['verdict']);
}
printf("ratios: blind/vlad-dispatch %s (frozen 2.2-2.3), worker/vlad-worker %s (frozen 8.0-10.7)\n",
    $summary['same_session_ratios']['blind/vladimir-dispatch'], $summary['same_session_ratios']['worker/vladimir-worker']);
