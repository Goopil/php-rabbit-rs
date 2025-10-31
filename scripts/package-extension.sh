#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 5 ]]; then
  cat <<'EOF' >&2
Usage: scripts/package-extension.sh <binary-path> <php-version> <platform> <release-version> <output-dir>

Example:
  scripts/package-extension.sh target/release/librabbit_rs.so 8.2 linux-gnu-x86_64 v0.1.0 dist
EOF
  exit 1
fi

binary_path="$1"
php_version="$2"
platform_id="$3"
release_version="$4"
output_dir="$5"

if [[ ! -f "$binary_path" ]]; then
  echo "Binary not found: $binary_path" >&2
  exit 2
fi

mkdir -p "$output_dir"
# Resolve the output directory to an absolute path so packaging works even when we cd
output_dir="$(cd "$output_dir" && pwd)"

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

case "$platform_id" in
  linux-*|darwin-*)
    ext="so"
    ;;
  windows-*)
    ext="dll"
    ;;
  *)
    ext="${binary_path##*.}"
    ;;
esac

binary_target="rabbit_rs.${ext}"
cp "$binary_path" "${tmp_dir}/${binary_target}"

cat > "${tmp_dir}/rabbit_rs.ini" <<EOF
; Automatically generated during packaging
extension=${binary_target}
EOF

cat > "${tmp_dir}/INSTALL.md" <<EOF
# RabbitRs PHP Extension — ${platform_id}

Version: ${release_version}
PHP ABI: ${php_version}

1. Copy \`${binary_target}\` into a directory listed in \`extension_dir\`.
2. Add \`extension=${binary_target}\` to \`php.ini\` or drop \`rabbit_rs.ini\` into conf.d.
3. Run \`php -m | grep rabbit_rs\` to confirm.
EOF

artifact_name="rabbit_rs-${platform_id}-php${php_version}.zip"
artifact_path="${output_dir}/${artifact_name}"

(cd "$tmp_dir" && zip -9 -q "${artifact_path}" "${binary_target}" rabbit_rs.ini INSTALL.md)

sha256sum "${artifact_path}" > "${artifact_path}.sha256"

echo "Created ${artifact_path}"
