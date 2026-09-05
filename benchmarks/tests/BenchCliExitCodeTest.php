<?php

declare(strict_types=1);

describe('bench.php CLI contract', function () {
    it('exits 2 without an output JSON when the broker is unreachable', function () {
        $outputPath = sys_get_temp_dir().'/bench-exit-code-'.getmypid().'.json';
        @unlink($outputPath);

        // amqplib at a dead port: no extension and no broker required — the
        // permanent-failure escape path is driver-agnostic (issue #141).
        $command = sprintf(
            'VLADIMIR_PORT=59999 %s %s --connection=rabbitmq-amqplib --mode=dispatch --count=2 --output=%s 2>&1',
            escapeshellarg(PHP_BINARY),
            escapeshellarg(__DIR__.'/../driver-bench/bin/bench.php'),
            escapeshellarg($outputPath),
        );
        exec($command, $lines, $exitCode);
        $output = implode("\n", $lines);

        expect($exitCode)->toBe(2, "bench.php must exit 2 on an unreachable broker (got {$exitCode}):\n{$output}");
        expect($outputPath)->not->toBeFile();
    });
});
