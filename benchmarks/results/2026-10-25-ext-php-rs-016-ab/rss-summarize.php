<?php
// Collapses RSS samples + /usr/bin/time logs into rss-metrics.json for one
// build (one raw-<label> dir). Usage: php rss-summarize.php <outdir> <label>
// Unit notes: ps rss is KB; macOS /usr/bin/time -l max RSS is bytes -> /1024.
$outdir = $argv[1];
$label = $argv[2];

$perScenario = [];
foreach (glob($outdir . '/rss-*.samples.txt') ?: [] as $file) {
    if (!preg_match('#/rss-([a-z-]+)-(\d+)\.samples\.txt$#', $file, $m)) {
        continue;
    }
    [$scenario, $ts] = [$m[1], $m[2]];
    $lines = file($file, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES);
    $rss = array_map(fn(string $l) => (int) explode(' ', trim($l))[1], $lines);
    if ($rss === []) {
        continue;
    }
    // drop the very first sample if it predates extension/module init spike
    $first = $rss[0];
    $peak = max($rss);
    $last = $rss[count($rss) - 1];
    $steady = array_slice($rss, (int) (count($rss) * 0.5)); // second half = steady state
    $perScenario[$scenario][] = [
        'ts' => (int) $ts,
        'samples' => count($rss),
        'first_kb' => $first,
        'peak_ps_kb' => $peak,
        'last_kb' => $last,
        'steady_median_kb' => median($steady),
        'delta_peak_minus_first_kb' => $peak - $first,
    ];
}

$timeMax = [];
foreach (glob($outdir . '/time-*.log') ?: [] as $file) {
    if (!preg_match('#/time-([a-z-]+)-(\d+)\.log$#', $file, $m)) {
        continue;
    }
    $content = (string) file_get_contents($file);
    if (preg_match('/([\d]+)\s+maximum resident set size/', $content, $m2)) {
        $timeMax[$m[1]][] = ['ts' => (int) $m[2], 'max_rss_kb' => (int) (((int) $m2[1]) / 1024)];
    }
}

$summary = [
    'label' => $label,
    'note' => 'ps rss in KB; /usr/bin/time -l maximum resident set size (macOS bytes) converted to KB. steady_median = median RSS over the second half of the run; delta = peak_ps - first sample.',
    'scenarios' => [],
];
foreach ($perScenario as $scenario => $runs) {
    $summary['scenarios'][$scenario] = [
        'invocations' => count($runs),
        'first_kb_median' => median(array_column($runs, 'first_kb')),
        'peak_ps_kb_median' => median(array_column($runs, 'peak_ps_kb')),
        'last_kb_median' => median(array_column($runs, 'last_kb')),
        'steady_median_kb_median' => median(array_column($runs, 'steady_median_kb')),
        'delta_peak_minus_first_kb_median' => median(array_column($runs, 'delta_peak_minus_first_kb')),
        'time_max_rss_kb_median' => median(array_column($timeMax[$scenario] ?? [], 'max_rss_kb')),
        'per_invocation' => $runs,
        'time_per_invocation' => $timeMax[$scenario] ?? [],
    ];
}
file_put_contents($outdir . '/rss-metrics.json', json_encode($summary, JSON_PRETTY_PRINT));
echo "wrote {$outdir}/rss-metrics.json\n";

function median(array $v): float
{
    if ($v === []) {
        return 0.0;
    }
    sort($v);
    $n = count($v);
    return $n % 2 ? $v[intdiv($n, 2)] : ($v[$n / 2 - 1] + $v[$n / 2]) / 2;
}
