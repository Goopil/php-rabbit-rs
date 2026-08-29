#!/usr/bin/env bash
set -euo pipefail

# update-homebrew-formula.sh - update the Homebrew tap formula with new release artifacts.
#
# Downloads macOS arm64 NTS artifacts for PHP 8.4 and 8.5 from GitHub Releases,
# computes SHA-256, clones the tap repo, updates Formula/rabbit-rs.rb, and pushes.
#
# Usage:
#   ./scripts/update-homebrew-formula.sh --version 0.0.7
#
# Environment:
#   MIRROR_TOKEN  GitHub token with write access to Goopil/homebrew-rabbit-rs

TAP_REPO="Goopil/homebrew-rabbit-rs"
RELEASE_BASE="https://github.com/Goopil/rabbit-rs/releases/download"
FORMULA_PATH="Formula/rabbit-rs.rb"

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
Usage: update-homebrew-formula.sh --version <ver>

Options:
  --version <ver>   Release version, e.g. 0.0.7
  -h, --help        Show this help

Environment:
  MIRROR_TOKEN  GitHub token with write access to Goopil/homebrew-rabbit-rs
USAGE
}

# --- argument parsing ---------------------------------------------------------

VERSION=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *)         fail "unknown argument: $1" ;;
    esac
done

[[ -n "${VERSION}" ]] || { usage; fail "--version is required"; }

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    fail "version '${VERSION}' is not semver X.Y.Z"
fi

ok "version: ${VERSION}"

# --- check prerequisites -------------------------------------------------------

if [[ -z "${MIRROR_TOKEN:-}" ]]; then
    fail "MIRROR_TOKEN environment variable is not set"
fi

if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
    fail "sha256sum or shasum is required"
fi

sha256_func() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}

if ! command -v ruby >/dev/null 2>&1; then
    fail "ruby is required to update the formula"
fi

if ! command -v unzip >/dev/null 2>&1; then
    fail "unzip is required to verify artifact contents"
fi

# --- download macOS artifacts and compute SHA-256 -----------------------------

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

declare -A SHAS

for php_ver in 8.4 8.5; do
    asset_name="php_rabbit_rs-v${VERSION}_php${php_ver}-arm64-darwin-nts.zip"
    download_url="${RELEASE_BASE}/v${VERSION}/${asset_name}"
    zip_path="${TMP_DIR}/${asset_name}"

    echo "==> Downloading ${asset_name}"
    # URL is HTTPS-only (-L follows redirects; GitHub release URLs never downgrade to HTTP).
    curl -fsSL --retry 3 --retry-delay 5 -o "${zip_path}" "${download_url}" || fail "failed to download ${download_url}" # NOSONAR

    sha="$(sha256_func "${zip_path}")"
    SHAS["${php_ver}"]="${sha}"
    ok "${asset_name} sha256=${sha}"

    # Verify the zip contains rabbit_rs.so
    if ! unzip -l "${zip_path}" | grep -q "rabbit_rs.so"; then
        fail "zip does not contain rabbit_rs.so: ${asset_name}"
    fi
done

# --- clone the tap repo -------------------------------------------------------

TAP_DIR="${TMP_DIR}/homebrew-rabbit-rs"
echo "==> Cloning ${TAP_REPO}"
git clone --depth 1 "https://${MIRROR_TOKEN}@github.com/${TAP_REPO}.git" "${TAP_DIR}" 2>&1 | sed 's|https://[^@]*@|https://|g'

# --- update the formula using ruby --------------------------------------------
#
# The formula uses a class-level url/sha256 for PHP 8.4 (required by Homebrew)
# and a resource "php85" block for PHP 8.5. The class-level lines are indented
# with exactly 2 spaces; the resource block lines are indented with 4 spaces,
# so the 2-space-anchored regexes only match the class-level fields.

RUBY_SCRIPT=$(cat <<RUBY
version = "${VERSION}"
sha84 = "${SHAS["8.4"]}"
sha85 = "${SHAS["8.5"]}"
formula_path = "${TAP_DIR}/${FORMULA_PATH}"

content = File.read(formula_path)

content.gsub!(/^  version ".*"/, "  version \"#{version}\"")

url84 = "https://github.com/Goopil/rabbit-rs/releases/download/v#{version}/php_rabbit_rs-v#{version}_php8.4-arm64-darwin-nts.zip"
content.gsub!(/^  url ".*"/, "  url \"#{url84}\"")
content.gsub!(/^  sha256 ".*"/, "  sha256 \"#{sha84}\"")

url85 = "https://github.com/Goopil/rabbit-rs/releases/download/v#{version}/php_rabbit_rs-v#{version}_php8.5-arm64-darwin-nts.zip"
content.gsub!(
  /resource "php85" do\n    url ".*"\n    sha256 ".*"/,
  "resource \"php85\" do\n    url \"#{url85}\"\n    sha256 \"#{sha85}\""
)

File.write(formula_path, content)
puts "Formula updated:"
puts "  version: #{version}"
puts "  php84 sha256: #{sha84}"
puts "  php85 sha256: #{sha85}"
RUBY
)

ruby -e "${RUBY_SCRIPT}"

# --- commit and push ----------------------------------------------------------

cd "${TAP_DIR}"
git config user.name "rabbit-rs-ci"
git config user.email "ci@rabbit-rs.local"
git add "${FORMULA_PATH}"
if ! git diff --cached --quiet; then
    git commit -m "Update formula to v${VERSION}"
    git push origin main 2>&1 | sed 's|https://[^@]*@|https://|g'
else
    echo "==> Formula already up to date for v${VERSION} -- nothing to commit"
fi

ok "formula updated to v${VERSION} and pushed to ${TAP_REPO}"
