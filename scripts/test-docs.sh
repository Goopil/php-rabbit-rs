#!/usr/bin/env bash
set -euo pipefail

# test-docs.sh — verify documentation integrity.
#
# Checks:
#   1. All expected doc files exist
#   2. README contains the two installation commands
#   3. All internal links resolve
#   4. No forbidden V1 distribution channel mentions (PECL, apt, rpm, apk as V1 channels)

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCS_DIR="${ROOT_DIR}/docs"
README="${ROOT_DIR}/README.md"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

ok() {
    echo "OK: $*"
}

echo "==> Checking required doc files exist"

required_files=(
    "${README}"
    "${DOCS_DIR}/installation.md"
    "${DOCS_DIR}/distribution.md"
    "${DOCS_DIR}/configuration.md"
    "${DOCS_DIR}/laravel.md"
    "${DOCS_DIR}/topology.md"
    "${DOCS_DIR}/reliability.md"
    "${DOCS_DIR}/operations.md"
    "${DOCS_DIR}/octane.md"
    "${DOCS_DIR}/troubleshooting.md"
    "${ROOT_DIR}/examples/laravel/Dockerfile"
    "${ROOT_DIR}/examples/laravel/example-job.php"
    "${ROOT_DIR}/examples/laravel/worker-supervisor.conf"
)

for file in "${required_files[@]}"; do
    if [[ ! -f "${file}" ]]; then
        fail "required file missing: ${file}"
    fi
done
ok "All ${#required_files[@]} required files exist"

echo "==> Checking README contains installation commands"

if ! grep -q 'pie install goopil/rabbit-rs-native' "${README}"; then
    fail "README does not contain 'pie install goopil/rabbit-rs-native'"
fi
ok "README contains PIE install command"

if ! grep -q 'composer require goopil/rabbit-rs-laravel' "${README}"; then
    fail "README does not contain 'composer require goopil/rabbit-rs-laravel'"
fi
ok "README contains composer require command"

echo "==> Checking internal links resolve"

# Extract all relative links from all markdown files and verify they resolve.
# Link patterns: [text](path/to/file.md) or [text](path/to/file.md#anchor)
link_pattern='\[([^]]*)\]\(([^)]+)\)'

broken_links=0
checked_links=0

check_link() {
    local source_file="$1"
    local link="$2"
    local source_dir
    source_dir="$(dirname "${source_file}")"

    # Skip external links (http, https, mailto)
    if [[ "${link}" =~ ^(https?|mailto): ]]; then
        return 0
    fi

    # Strip anchor from the link
    local target="${link%%#*}"

    # Skip empty targets (anchor-only links)
    if [[ -z "${target}" ]]; then
        return 0
    fi

    # Resolve relative to the source file directory
    local resolved="${source_dir}/${target}"

    # Normalize the path
    resolved="$(cd "${source_dir}" 2>/dev/null && realpath -q "${target}" 2>/dev/null || echo "${resolved}")"

    if [[ ! -e "${resolved}" ]]; then
        echo "  BROKEN: ${source_file#${ROOT_DIR}/} -> ${link}"
        broken_links=$((broken_links + 1))
    fi
    checked_links=$((checked_links + 1))
}

# Find all markdown files
md_files=()
while IFS= read -r -d '' f; do
    md_files+=("${f}")
done < <(find "${ROOT_DIR}" -name '*.md' -not -path '*/vendor/*' -not -path '*/node_modules/*' -not -path '*/.git/*' -print0)

for md_file in "${md_files[@]}"; do
    # Read file line by line and extract links
    while IFS= read -r line; do
        [[ -z "${line}" ]] && continue
        # Use grep to find links in the line
        while IFS= read -r match; do
            [[ -z "${match}" ]] && continue
            # Extract the URL part from [text](url)
            link="${match#*\(}"
            link="${link%\)*}"
            check_link "${md_file}" "${link}"
        done < <(echo "${line}" | grep -oE '\[[^]]*\]\([^)]+\)' 2>/dev/null || true)
    done < "${md_file}"
done

if [[ "${broken_links}" -gt 0 ]]; then
    fail "Found ${broken_links} broken link(s) out of ${checked_links} checked"
fi
ok "All ${checked_links} internal links resolve"

echo "==> Checking for forbidden V1 distribution channel mentions"

# We check that PECL, apt, rpm, apk are not presented as V1 distribution channels.
# These words may appear in the "Not V1 distribution channels" sections, which
# explicitly state they are NOT channels. So we look for patterns where these
# are presented as installation methods, not as exclusions.
#
# Forbidden patterns (in context of "install via"):
#   "pecl install" (as a recommendation)
#   "apt install" or "apt-get install" (for rabbit-rs)
#   "yum install" or "dnf install" (for rabbit-rs)
#   "apk add" (for rabbit-rs)
#
# The docs explicitly state these are NOT V1 channels, so the words appear
# in exclusion lists. We check for active install recommendations instead.

forbidden_patterns=(
    "Install via PECL"
    "Install with pecl"
    "Install using apt"
    "Install using yum"
    "Install using dnf"
    "Install using apk"
    "pecl install rabbit"
    "apt-get install rabbit-rs"
    "yum install rabbit-rs"
    "dnf install rabbit-rs"
    "apk add rabbit-rs"
)

forbidden_found=0
for md_file in "${md_files[@]}"; do
    for pattern in "${forbidden_patterns[@]}"; do
        if grep -qi "${pattern}" "${md_file}" 2>/dev/null; then
            # Check if this is in an exclusion context
            # Look at the surrounding lines for "not" or "not supported"
            context_lines=$(grep -B 5 -A 2 -i "${pattern}" "${md_file}" 2>/dev/null || true)
            if ! echo "${context_lines}" | grep -qiE '(not (a |an )?(V1|distribution|supported|channel)|out of scope|not maintained|not provided)'; then
                echo "  FORBIDDEN: ${md_file#${ROOT_DIR}/} contains '${pattern}' outside exclusion context"
                forbidden_found=$((forbidden_found + 1))
            fi
        fi
    done
done

if [[ "${forbidden_found}" -gt 0 ]]; then
    fail "Found ${forbidden_found} forbidden V1 channel mention(s)"
fi
ok "No forbidden V1 distribution channel mentions"

echo ""
echo "All documentation checks passed."
