#!/usr/bin/env bash
#
# Docs coherence lint (v1 readiness remediation plan, WS7): fails when stale
# contract claims appear in the maintained documentation.
#
# The scanned file list is explicit: CHANGELOG.md, docs/plans/ROADMAP.md,
# docs/audit/ and other historical records are deliberately excluded — stale
# claims are allowed to live there by design. Also cross-checks the metric
# names referenced by docs/operations/alerts.md against the core metrics
# registry (skipped silently until that deliverable exists).

set -uo pipefail

cd "$(dirname "$0")/.."

FILES=(
  README.md
  docs/reference.md
  docs/plans/2026-07-30-rabbitmq-native-design.md
  docs/plans/2026-07-30-rabbitmq-native-implementation.md
  packages/laravel-queue/docs/reference.md
  packages/laravel-queue/docs/getting-started.md
)

# Stale claims: the support contract these phrases state is no longer true.
# - the distribution is NTS-only, 10 archives / 30 assets (2026-08-31);
# - the supported broker floor is RabbitMQ 4.2.9+;
# - delay.mode=auto is a documented alias for the plugin driver, with no
#   TTL fallback.
PATTERNS=(
  '16 release archives'
  'RabbitMQ 4\.3'
  'NTS and ZTS'
  'plugin when available'
  'TTL buckets otherwise'
)

# Require-model claims: `ext-rabbit_rs` is a Composer *suggestion* enforced
# by a typed runtime error at connection resolution — composer.json does not
# require it and Composer never verifies it at install time.
REQUIRE_MODEL_PATTERNS=(
  'Composer to check'
  'Composer checks for the extension'
  'verifies .{0,10}`?ext-rabbit_rs`?.{0,3} is loaded'
  'requires a specific .{0,3}`?ext-rabbit_rs'
  'package requires .{0,3}`?ext-rabbit_rs'
  'ext-rabbit_rs.{0,4}\^0\.1'
)

failures=0

for file in "${FILES[@]}"; do
  if [ ! -f "$file" ]; then
    echo "check-docs: missing maintained doc: $file" >&2
    failures=$((failures + 1))
    continue
  fi
  for pattern in "${PATTERNS[@]}" "${REQUIRE_MODEL_PATTERNS[@]}"; do
    while IFS= read -r match; do
      echo "check-docs: stale claim in $file:$match" >&2
      failures=$((failures + 1))
    done < <(grep -nE -- "$pattern" "$file")
  done
done

# Metric cross-check: every `*_total` metric named in the operations alert
# rules must exist in the core metrics registry. The operations pack
# (runbook/alerts/dashboard) is a later remediation deliverable, so its
# absence is not an error.
if [ -f docs/operations/alerts.md ]; then
  for metric in $(grep -oE '`[a-z][a-z0-9_]*_total`' docs/operations/alerts.md | tr -d '`' | sort -u); do
    if ! grep -q "${metric}" crates/rabbit-rs-core/src/metrics.rs; then
      echo "check-docs: metric ${metric} referenced by docs/operations/alerts.md is missing from crates/rabbit-rs-core/src/metrics.rs" >&2
      failures=$((failures + 1))
    fi
  done
fi

if [ "$failures" -gt 0 ]; then
  echo "check-docs: ${failures} stale doc claim(s) found. Update the docs to the current contract (annotate history instead of deleting it)." >&2
  exit 1
fi

echo "check-docs: OK (no stale contract claims)"
