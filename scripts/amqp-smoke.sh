#!/bin/sh
# AMQP functional smoke for the rabbit_rs extension (issue #227).
#
# Loads the extension, publishes + confirms 5 messages, consumes and acks
# them, then runs one Toxiproxy outage/recovery scenario (publish 2 during
# an outage, assert they buffer and confirm after the heal). Any loss
# exits non-zero. The proxy created for the recovery phase is always
# deleted, so repeated runs are idempotent.
#
# Usage:
#   scripts/amqp-smoke.sh [--php <php-binary>] [--ext <extension-artifact>]
#       [--dsn <host:port>] [--toxiproxy <api-url>] [--skip-recovery]
#
# Defaults: php on PATH, extension loaded from PHP's own configuration
# (PIE installs), DSN 127.0.0.1:5672 (the lab's published port), Toxiproxy
# API http://127.0.0.1:18474. Broker vhost/credentials default to the lab's
# (vhost "/", user rabbit_rs / rabbit_rs_lab); override with the
# RABBIT_RS_SMOKE_VHOST / _USER / _PASSWORD environment variables.
#
# Requires the lab: ./scripts/lab-up.sh with-plugin
set -u

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

fail() {
    echo "[amqp-smoke] FAIL: $1" >&2
    exit 1
}

PHP_BIN=""
EXT=""
DSN=""
TOXIPROXY=""
SKIP_RECOVERY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --php) [ $# -ge 2 ] || fail "--php needs a value"; PHP_BIN="$2"; shift 2 ;;
        --ext) [ $# -ge 2 ] || fail "--ext needs a value"; EXT="$2"; shift 2 ;;
        --dsn) [ $# -ge 2 ] || fail "--dsn needs a value"; DSN="$2"; shift 2 ;;
        --toxiproxy) [ $# -ge 2 ] || fail "--toxiproxy needs a value"; TOXIPROXY="$2"; shift 2 ;;
        --skip-recovery) SKIP_RECOVERY=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) fail "unknown argument '$1' (see --help)" ;;
    esac
done

[ -n "$PHP_BIN" ] || PHP_BIN=php
command -v "$PHP_BIN" >/dev/null 2>&1 || fail "php binary not found: $PHP_BIN"

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd) || fail "cannot resolve script directory"
DRIVER="$SCRIPT_DIR/amqp-smoke/run.php"
[ -f "$DRIVER" ] || fail "driver not found: $DRIVER (broken checkout?)"

# Step (a): the extension must load and report itself before anything else
# touches AMQP. Without --ext the extension must already be configured
# (PIE installs via ini); with --ext it is loaded explicitly.
set --
if [ -n "$EXT" ]; then
    [ -f "$EXT" ] || fail "extension artifact not found: $EXT"
    set -- "$@" -d "extension=$EXT"
fi
OUT=$("$PHP_BIN" "$@" -r 'var_dump(extension_loaded("rabbit_rs"));' 2>&1) ||
    fail "php exited non-zero while loading the extension: $OUT"
case "$OUT" in
    *"bool(true)"*) echo "[amqp-smoke] extension loaded: OK" ;;
    *) fail "the rabbit_rs extension did not load: $OUT" ;;
esac

# Phases (b)-(f) live in the PHP driver: publish + confirms, consume + ack,
# Toxiproxy outage/recovery, non-zero exit on any loss. PHP CLI options
# (--ext) must precede the driver path; anything after it is passed to
# run.php via $argv. A --dsn=... before the script path would be parsed as
# a PHP option and rejected with a usage error.
set --
if [ -n "$EXT" ]; then
    set -- "$@" -d "extension=$EXT"
fi
set -- "$@" "$DRIVER"
[ -n "$DSN" ] && set -- "$@" "--dsn=$DSN"
[ -n "$TOXIPROXY" ] && set -- "$@" "--toxiproxy=$TOXIPROXY"
[ -n "$SKIP_RECOVERY" ] && set -- "$@" --skip-recovery

"$PHP_BIN" "$@"
