#!/usr/bin/env bash
set -euo pipefail

# trigger-packagist-update.sh — ask Packagist to re-crawl a package repository.
#
# Usage:
#   ./scripts/trigger-packagist-update.sh --repository https://github.com/Goopil/rabbit-rs
#
# Environment:
#   PACKAGIST_TOKEN  Packagist API token. Sent in an Authorization header read
#                    from a 0600 temp file so it never appears in a URL, curl
#                    error output, or process listings.

PACKAGIST_USER="goopil"
PACKAGIST_API_URL="https://packagist.org/api/update-package"

# --- helpers ------------------------------------------------------------------

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

usage() {
    cat <<'USAGE'
Usage: trigger-packagist-update.sh --repository <url>

Options:
  --repository <url>  Repository URL registered on Packagist,
                      e.g. https://github.com/Goopil/rabbit-rs
  -h, --help          Show this help

Environment:
  PACKAGIST_TOKEN  Packagist API token (sent via Authorization header, never in a URL)
USAGE
}

# --- argument parsing ---------------------------------------------------------

REPOSITORY=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repository) REPOSITORY="$2"; shift 2 ;;
        -h|--help)    usage; exit 0 ;;
        *)            fail "unknown argument: $1" ;;
    esac
done

[[ -n "${REPOSITORY}" ]] || { usage; fail "--repository is required"; }
[[ -n "${PACKAGIST_TOKEN:-}" ]] || fail "PACKAGIST_TOKEN environment variable is not set"

for cmd in curl jq; do
    command -v "${cmd}" >/dev/null 2>&1 || fail "required command not found: ${cmd}"
done

# --- trigger the update -------------------------------------------------------

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

HEADER_FILE="${TMP_DIR}/authorization-header"
printf 'Authorization: Bearer %s:%s\n' "${PACKAGIST_USER}" "${PACKAGIST_TOKEN}" > "${HEADER_FILE}"
chmod 600 "${HEADER_FILE}"
BODY_FILE="${TMP_DIR}/response.json"

echo "==> Triggering Packagist update for ${REPOSITORY}"
# --fail turns 4xx/5xx (revoked token, unknown user) into a nonzero exit so a
# failed Packagist update fails the release loudly instead of silently.
curl --silent --show-error --fail \
    --header "@${HEADER_FILE}" \
    --header "Content-Type: application/json" \
    --data "{\"repository\":{\"url\":\"${REPOSITORY}\"}}" \
    --output "${BODY_FILE}" \
    "${PACKAGIST_API_URL}"

# A 2xx status alone is not enough: assert the expected body so an unexpected
# response is caught before the release is declared done.
status="$(jq -r '.status // empty' "${BODY_FILE}")"
if [[ "${status}" != "success" ]]; then
    fail "Packagist update-package did not succeed (status: '${status:-<missing>}'). Response: $(cat "${BODY_FILE}")"
fi

ok "Packagist update triggered for ${REPOSITORY}"
