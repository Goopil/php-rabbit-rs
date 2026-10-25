#!/usr/bin/env bash
# One A/B cell: one harness invocation for the rabbit-rs driver against a
# given extension cdylib, with per-100ms RSS sampling of the PHP process and
# /usr/bin/time -l max RSS. Writes:
#   <outdir>/partial-rabbit-rs.json   (harness partial, appends runs)
#   <outdir>/rss-<scenario>-<ts>.samples.txt   ("epoch_seconds rss_kb" lines)
#   <outdir>/time-<scenario>-<ts>.log          (/usr/bin/time -l output)
#   <outdir>/run-<scenario>-<ts>.log           (harness stdout)
#
#   rss-run.sh <dylib> <scenario> <outdir> <warmup 0|1>
set -euo pipefail

DYLIB="$1"; SCENARIO="$2"; OUTDIR="$3"; WARMUP="${4:-0}"
BENCH_DIR="$(cd "$(dirname "$0")/../../.." && pwd)/benchmarks"
TS="$(date +%s)"
mkdir -p "$OUTDIR"

/usr/bin/time -l php -d xdebug.mode=off -d extension="$DYLIB" \
    "${BENCH_DIR}/src/run-comparison.php" \
    --driver=rabbit-rs --scenario="$SCENARIO" --rounds=1 --warmup="$WARMUP" \
    --kill-container=rabbitrs-rabbitmq-1-1 --output-dir="$OUTDIR" \
    > "$OUTDIR/run-$SCENARIO-$TS.log" 2> "$OUTDIR/time-$SCENARIO-$TS.log" &
TPID=$!

# Sample the PHP process (direct child of /usr/bin/time) every 100 ms.
SAMPLES="$OUTDIR/rss-$SCENARIO-$TS.samples.txt"
: > "$SAMPLES"
while kill -0 "$TPID" 2>/dev/null; do
    PHP_PID="$(pgrep -P "$TPID" 2>/dev/null || true)"
    if [[ -n "$PHP_PID" ]]; then
        RSS="$(ps -o rss= -p "$PHP_PID" 2>/dev/null | tr -d ' ' || true)"
        if [[ -n "$RSS" && "$RSS" != "0" ]]; then
            printf '%s %s\n' "$(date +%s)" "$RSS" >> "$SAMPLES"
        fi
    fi
    sleep 0.1
done

wait "$TPID"
