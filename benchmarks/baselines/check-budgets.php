#!/usr/bin/env php
<?php

declare(strict_types=1);

/*
|--------------------------------------------------------------------------
| RC budget checker — anti-regression gate against the reference baseline
|--------------------------------------------------------------------------
|
| Compares benchmark result JSONs against the committed baseline
| (reference-machine.json, same directory) for the RC pipeline:
|
|   - publish/consume throughput >= throughput_min_ratio (default 0.8) of baseline
|   - publish/consume per-op p99 <= p99_max_ratio (default 1.5) of baseline
|   - ALWAYS blocking, regardless of thresholds: the run's own integrity
|     verdict (`ok`), losses (soak reports `missing`), duplicates, the final
|     `publish_buffered` reading, and the soak buffered-tripwire counter.
|
| Accepts one result JSON file or one directory of them. Both result schemas
| published by this repo are understood (see the metric contract in
| benchmarks/README.md):
|
|   - driver-bench cells (bench.php — what rebench-driver-bench.sh archives):
|     publish metrics from dispatch cells, consume metrics from worker cells.
|   - soak runs (soak.php): no throughput/latency metrics exist there, so the
|     ratio checks are n/a and the always-blocking checks carry the verdict.
|
| Rows marked "n/a" mean "not measured for this run" (a null metric is never
| read as zero) or "no baseline recorded for this scenario" — n/a never
| counts as passing. A run in which not a single check could be applied
| fails loudly instead of passing silently.
|
| Exit codes:
|   0 — every applied check passed
|   1 — at least one check failed, or data could not be judged
|       (unrecognized schema, unparseable JSON), or no check applied
|   2 — usage errors (bad arguments, unreadable baseline, no results found)
*/

const DEFAULT_THROUGHPUT_RATIO = 0.8;
const DEFAULT_P99_RATIO = 1.5;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

$resultsPath = null;
$baselinePath = __DIR__.'/reference-machine.json';
$thresholdsPath = null;

foreach (array_slice($argv ?? [], 1) as $arg) {
    if (preg_match('/^--([a-z0-9-]+)=(.*)$/i', (string) $arg, $m) === 1) {
        match ($m[1]) {
            'baseline' => $baselinePath = $m[2],
            'thresholds' => $thresholdsPath = $m[2],
            default => usage("unknown option --{$m[1]}"),
        };
        continue;
    }

    if ($resultsPath !== null) {
        usage('exactly one results path (file or directory) is accepted');
    }
    $resultsPath = $arg;
}

if ($resultsPath === null) {
    usage('missing results path (file or directory)');
}

// ---------------------------------------------------------------------------
// Baseline + thresholds
// ---------------------------------------------------------------------------

$baseline = load_json_file($baselinePath);
if (! is_array($baseline)) {
    usage("baseline not readable or not a JSON object: {$baselinePath}");
}

$baselines = is_array($baseline['baselines'] ?? null) ? $baseline['baselines'] : [];

$thresholds = [
    'throughput_min_ratio' => DEFAULT_THROUGHPUT_RATIO,
    'p99_max_ratio' => DEFAULT_P99_RATIO,
];
if (is_array($baseline['thresholds'] ?? null)) {
    $thresholds = array_merge($thresholds, $baseline['thresholds']);
}
if ($thresholdsPath !== null) {
    $override = load_json_file($thresholdsPath);
    if (! is_array($override)) {
        usage("threshold file not readable or not a JSON object: {$thresholdsPath}");
    }
    $thresholds = array_merge($thresholds, $override);
}

foreach (['throughput_min_ratio', 'p99_max_ratio'] as $key) {
    if (! is_numeric($thresholds[$key] ?? null) || (float) $thresholds[$key] <= 0.0) {
        usage("threshold {$key} must be a positive number");
    }
}
$throughputRatio = (float) $thresholds['throughput_min_ratio'];
$p99Ratio = (float) $thresholds['p99_max_ratio'];

// ---------------------------------------------------------------------------
// Result files
// ---------------------------------------------------------------------------

if (is_file($resultsPath)) {
    $files = [$resultsPath];
} elseif (is_dir($resultsPath)) {
    $files = glob($resultsPath.'/*.json') ?: [];
    sort($files);
    if ($files === []) {
        usage("no result JSONs found in directory: {$resultsPath}");
    }
} else {
    usage("results path not found: {$resultsPath}");
}

// ---------------------------------------------------------------------------
// Check every file, collect table rows
// ---------------------------------------------------------------------------

$rows = [];
$failed = 0;
$passed = 0;
$na = 0;
$byScenario = [];

/** Append a row and count its verdict (last column). */
$emit = function (array $row) use (&$rows, &$passed, &$failed, &$na): void {
    $rows[] = $row;
    match (end($row)) {
        'PASS' => $passed++,
        'FAIL' => $failed++,
        default => $na++,
    };
};

foreach ($files as $file) {
    $name = basename($file);
    $decoded = load_json_file($file);

    if (! is_array($decoded)) {
        $emit([$name, '-', 'json', 'unparseable or unreadable', '-', 'FAIL']);
        continue;
    }

    $normalized = normalize_result($decoded);

    if ($normalized === null) {
        $emit([$name, '-', 'schema', 'unrecognized', '-', 'FAIL']);
        continue;
    }

    $scenario = $normalized['scenario'] ?? '-';

    // --- Always blocking: run integrity + integrity counters -----------------

    foreach (
        [
            ['ok', 'run integrity (ok)'],
            ['losses', 'losses'],
            ['duplicates', 'duplicates'],
            ['publish_buffered', 'publish_buffered (final)'],
            ['tripwire_failures', 'buffered tripwire failures'],
        ] as [$key, $label]
    ) {
        $actual = $normalized[$key];

        if ($key === 'ok') {
            $emit([$name, $scenario, $label, $actual === null ? 'n/a' : ($actual ? 'true' : 'false'), 'true', $actual === null ? 'n/a' : ($actual ? 'PASS' : 'FAIL')]);
            continue;
        }

        $emit([$name, $scenario, $label, $actual === null ? 'n/a' : (string) $actual, '== 0', $actual === null ? 'n/a' : ($actual === 0 ? 'PASS' : 'FAIL')]);
    }

    // --- Collect for the per-scenario ratio checks ---------------------------

    if ($normalized['ratio_kind'] === null) {
        continue;
    }

    $byScenario[$normalized['scenario']]['kind'] ??= $normalized['ratio_kind'];
    foreach (['throughput', 'p99_ms'] as $metricKey) {
        if ($normalized[$metricKey] !== null) {
            $byScenario[$normalized['scenario']]['values'][$metricKey][] = $normalized[$metricKey];
        }
    }
}

// ---------------------------------------------------------------------------
// Ratio checks per scenario: the median across the runs in this invocation is
// compared against the baseline. A single noise-contaminated run does not move
// the median; a real regression shifts every run and trips the budget.
// ---------------------------------------------------------------------------

foreach ($byScenario as $scenario => $grouped) {
    $base = isset($baselines[$scenario]) ? $baselines[$scenario] : null;
    $runs = count($grouped['values']['throughput'] ?? []);

    foreach (
        [
            ['throughput', 'throughput_ops_s', "{$grouped['kind']} throughput (ops/s, median of {$runs})", '>=', $throughputRatio],
            ['p99_ms', 'p99_ms', "{$grouped['kind']} p99 (ms, median of {$runs})", '<=', $p99Ratio],
        ] as [$metricKey, $baseKey, $label, $op, $ratio]
    ) {
        if ($base === null) {
            $emit([$scenario, $scenario, $label, 'n/a', $op.' baseline', 'n/a']);
            continue;
        }

        if (! is_numeric($base[$baseKey] ?? null)) {
            $emit([$scenario, $scenario, $label, 'n/a', 'malformed baseline entry', 'FAIL']);
            continue;
        }

        $budget = (float) $base[$baseKey] * $ratio;
        $actual = median_of($grouped['values'][$metricKey] ?? []);

        if ($actual === null) {
            $emit([$scenario, $scenario, $label, 'not measured', $op.' '.fmt_float($budget, $metricKey), 'FAIL']);
            continue;
        }

        $ok = $op === '>=' ? $actual >= $budget : $actual <= $budget;
        $emit([$scenario, $scenario, $label, fmt_float($actual, $metricKey), $op.' '.fmt_float($budget, $metricKey), $ok ? 'PASS' : 'FAIL']);
    }
}

// ---------------------------------------------------------------------------
// Table + verdict
// ---------------------------------------------------------------------------

$headers = ['result', 'scenario', 'metric', 'actual', 'budget', 'verdict'];
$widths = [];
foreach ($headers as $i => $header) {
    $widths[$i] = strlen($header);
    foreach ($rows as $row) {
        $widths[$i] = max($widths[$i], strlen((string) $row[$i]));
    }
}

echo 'RC budget check — baseline: ', $baselinePath, "\n";
echo 'thresholds: throughput >= ', fmt_ratio($throughputRatio), 'x baseline, p99 <= ',
    fmt_ratio($p99Ratio), 'x baseline; ok/losses/duplicates/publish_buffered always blocking', "\n\n";

foreach ($headers as $i => $header) {
    echo str_pad($header, $widths[$i]), '  ';
}
echo "\n", str_repeat('-', array_sum($widths) + 2 * count($widths)), "\n";

foreach ($rows as $row) {
    foreach ($row as $i => $cell) {
        echo str_pad((string) $cell, $widths[$i]), '  ';
    }
    echo "\n";
}

echo "\n";

if ($failed > 0) {
    echo "BUDGET: FAIL — {$failed} check(s) failed, {$passed} passed, {$na} n/a\n";
    exit(1);
}

if ($passed === 0) {
    echo "BUDGET: FAIL — no check could be applied ({$na} n/a); refusing to pass silently\n";
    exit(1);
}

echo "BUDGET: PASS — {$passed} check(s) passed, {$na} n/a\n";
exit(0);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Map a result JSON onto the checker's normalized metric set; null when the
 * schema is neither driver-bench nor soak. A null metric stays null: it means
 * "not measured for this run", never zero.
 *
 * @return array{scenario: ?string, ratio_kind: ?string, ok: ?bool, losses: ?int, duplicates: ?int, publish_buffered: ?int, tripwire_failures: ?int, throughput: ?float, p99_ms: ?float}|null
 */
function normalize_result(array $d): ?array
{
    $benchmark = $d['benchmark'] ?? null;

    if ($benchmark === 'driver-bench' || (isset($d['connection'], $d['mode']))) {
        $connection = (string) $d['connection'];
        $mode = (string) $d['mode'];
        $safety = $d['config']['rabbit_rs_global']['safety'] ?? $d['config']['safety'] ?? null;

        $scenario = match (true) {
            $connection === 'rabbit-rs' && $mode === 'dispatch' => 'goopil-dispatch'.(in_array($safety, ['blind', 'safe'], true) ? '-'.$safety : ''),
            $connection === 'rabbit-rs' && $mode === 'worker' => 'goopil-worker',
            $connection === 'rabbitmq-amqplib' => 'vladimir-'.$mode,
            default => null,
        };

        return [
            'scenario' => $scenario,
            'ratio_kind' => $mode === 'dispatch' ? 'publish' : ($mode === 'worker' ? 'consume' : null),
            'ok' => isset($d['ok']) ? (bool) $d['ok'] : null,
            'losses' => as_int($d['losses'] ?? null),
            'duplicates' => as_int($d['duplicates'] ?? null),
            'publish_buffered' => null,
            'tripwire_failures' => null,
            'throughput' => as_float($d['avg_rate_ops_s'] ?? null),
            'p99_ms' => as_float($d['latency_ms']['p99'] ?? null),
        ];
    }

    if ($benchmark === 'soak' || (array_key_exists('missing', $d) && isset($d['memory']))) {
        $samples = is_array($d['memory']['samples'] ?? null) ? $d['memory']['samples'] : [];
        $last = $samples !== [] ? $samples[count($samples) - 1] : null;

        return [
            'scenario' => 'soak',
            'ratio_kind' => null,
            'ok' => isset($d['ok']) ? (bool) $d['ok'] : null,
            'losses' => as_int($d['missing'] ?? null),
            'duplicates' => as_int($d['duplicates'] ?? null),
            'publish_buffered' => as_int($last['stats']['publish_buffered'] ?? null),
            'tripwire_failures' => as_int($d['memory']['buffered_tripwire_failures'] ?? null),
            'throughput' => null,
            'p99_ms' => null,
        ];
    }

    return null;
}

function as_int(mixed $value): ?int
{
    if (is_int($value)) {
        return $value;
    }
    if (is_float($value) && floor($value) === $value) {
        return (int) $value;
    }

    return null;
}

/** Median of a numeric list; null when empty. */
function median_of(array $values): ?float
{
    if ($values === []) {
        return null;
    }

    sort($values);
    $mid = intdiv(count($values), 2);

    return count($values) % 2 === 1
        ? (float) $values[$mid]
        : ($values[$mid - 1] + $values[$mid]) / 2.0;
}

function as_float(mixed $value): ?float
{
    return is_int($value) || is_float($value) ? (float) $value : null;
}

function fmt_float(float $value, string $metricKey): string
{
    return number_format($value, $metricKey === 'p99_ms' ? 3 : 2, '.', '');
}

function fmt_ratio(float $ratio): string
{
    $formatted = rtrim(rtrim(number_format($ratio, 4, '.', ''), '0'), '.');

    return $formatted === '' ? '0' : $formatted;
}

function load_json_file(string $path): mixed
{
    $raw = @file_get_contents($path);
    if ($raw === false) {
        return null;
    }

    try {
        return json_decode($raw, true, 512, JSON_THROW_ON_ERROR);
    } catch (JsonException) {
        return null;
    }
}

function usage(string $error): never
{
    fwrite(STDERR, "error: {$error}\n\n");
    fwrite(STDERR, "usage: php check-budgets.php <results-file-or-dir> [--baseline=PATH] [--thresholds=PATH]\n");
    fwrite(STDERR, "  --baseline    reference-machine JSON (default: reference-machine.json next to this script)\n");
    fwrite(STDERR, "  --thresholds  JSON overriding {\"throughput_min_ratio\": .., \"p99_max_ratio\": ..} from the baseline\n");

    exit(2);
}
