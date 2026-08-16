#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(dirname "$SCRIPT_DIR")"
RESULTS_DIR="${APP_DIR}/results"
RESULTS_FILE="${RESULTS_DIR}/matrix-$(date +%Y%m%d-%H%M%S).json"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

SMOKE=false
FULL=false
VERIFY_BUDGET=""
MODE="${BENCH_MODE:-cli}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --smoke)
            SMOKE=true
            shift
            ;;
        --full)
            FULL=true
            shift
            ;;
        --mode)
            MODE="$2"
            shift 2
            ;;
        --verify-budget)
            VERIFY_BUDGET="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--smoke|--full] [--mode <cli|fpm|octane>] [--verify-budget <file>]"
            exit 1
            ;;
    esac
done

if [[ "$SMOKE" == "false" && "$FULL" == "false" ]]; then
    SMOKE=true
fi

if [[ "$SMOKE" == "true" ]]; then
    DRIVERS=("database")
    PAYLOAD_SIZES=("256")
    BATCH_SIZES=("1")
    MSG_COUNT=50
else
    DRIVERS=("rabbit-rs" "php-amqplib" "vyuldashev" "redis" "database")
    PAYLOAD_SIZES=(256 1024 10240 102400)
    BATCH_SIZES=(1 16 64 256)
    MSG_COUNT=5000
fi

mkdir -p "$RESULTS_DIR"

ENV_FILE="$TMP_DIR/env.json"
php -r "
echo json_encode([
    'php_version' => PHP_VERSION,
    'sapi' => PHP_SAPI,
    'os' => PHP_OS_FAMILY,
    'os_release' => php_uname('r'),
    'machine' => php_uname('m'),
    'timestamp' => date('c'),
]);
" > "$ENV_FILE"

RESULTS_FILE_CONTENT="$TMP_DIR/results.json"
echo "[" > "$RESULTS_FILE_CONTENT"
FIRST=true

for driver in "${DRIVERS[@]}"; do
    for payload in "${PAYLOAD_SIZES[@]}"; do
        for batch in "${BATCH_SIZES[@]}"; do
            echo "Running: $driver payload=$payload batch=$batch count=$MSG_COUNT" >&2

            PUBLISH_FILE="$TMP_DIR/publish_${driver}_${payload}_${batch}.json"
            CONSUME_FILE="$TMP_DIR/consume_${driver}_${payload}_${batch}.json"

            "$APP_DIR/artisan" publish \
                --driver="$driver" \
                --count="$MSG_COUNT" \
                --payload-size="$payload" \
                --batch-size="$batch" \
                --mode="$MODE" > "$PUBLISH_FILE" 2>&1 || true

            if [[ ! -s "$PUBLISH_FILE" ]]; then
                echo '{}' > "$PUBLISH_FILE"
            fi

            "$APP_DIR/artisan" consume \
                --driver="$driver" \
                --count="$MSG_COUNT" \
                --payload-size="$payload" \
                --batch-size="$batch" \
                --mode="$MODE" > "$CONSUME_FILE" 2>&1 || true

            if [[ ! -s "$CONSUME_FILE" ]]; then
                echo '{}' > "$CONSUME_FILE"
            fi

            ENTRY=$(php "$SCRIPT_DIR/merge-result.php" \
                "$PUBLISH_FILE" \
                "$CONSUME_FILE" \
                "$driver" \
                "$payload" \
                "$batch" \
                "$MSG_COUNT" 2>/dev/null || echo '{}')

            if [[ "$FIRST" == "true" ]]; then
                FIRST=false
            else
                echo "," >> "$RESULTS_FILE_CONTENT"
            fi
            echo "$ENTRY" >> "$RESULTS_FILE_CONTENT"
        done
    done
done
echo "]" >> "$RESULTS_FILE_CONTENT"

php -r "
\$env = json_decode(file_get_contents('$ENV_FILE'), true);
\$results = json_decode(file_get_contents('$RESULTS_FILE_CONTENT'), true);
echo json_encode([
    'environment' => \$env,
    'results' => \$results,
], JSON_PRETTY_PRINT);
" > "$RESULTS_FILE"

cat "$RESULTS_FILE"
echo "" >&2
echo "Results saved to $RESULTS_FILE" >&2
