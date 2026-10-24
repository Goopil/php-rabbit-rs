#!/bin/sh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="${ROOT_DIR}/crates/rabbit-rs-php/Cargo.toml"

if [ ! -f "${MANIFEST}" ]; then
    echo "Cargo manifest not found: ${MANIFEST}" >&2
    exit 1
fi

# cargo-php requires a package manifest, not a workspace manifest.
# The root Cargo.toml is workspace-only, so we point at the extension crate.
# cargo-php >= 0.1.21 dlopens the built cdylib to read its metadata: no PHP
# embed SAPI is needed. On macOS, install cargo-php with:
#   RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php
# The generated stub is crates/rabbit-rs-php/stubs/rabbit_rs.stub.php;
# its docblocks are maintained in the Rust /// docs of src/classes/*.rs.

# Detect the invocation mode without disturbing "$@": --stdout prints the
# stub, -o/--out <path> writes it, and the cargo-php default writes
# <ext-name>.stubs.php in the current directory.
out_path=""
stdout_mode=0
prev=""
for arg in "$@"; do
    if [ "${prev}" = "-o" ] || [ "${prev}" = "--out" ]; then
        out_path="${arg}"
    else
        case "${arg}" in
            --stdout) stdout_mode=1 ;;
            -o?*) out_path="${arg#-o}" ;;
        esac
    fi
    prev="${arg}"
done

# Strips the macro-generated duplicate `@return` tag that cargo-php appends
# after a /// docblock that already declares its own `@return` shape, plus
# the blank comment lines the removal leaves at the end of the docblock.
# The appended tag always sits last in the docblock and repeats the runtime
# signature type (e.g. `array`), which would shadow the precise PHPDoc
# array shape in IDEs. Docblocks with a single `@return` (the macro one) are
# left untouched.
filter_stub() {
    awk '
        { lines[NR] = $0 }
        END {
            in_doc = 0
            start = 0
            for (i = 1; i <= NR; i++) {
                if (in_doc == 0 && lines[i] ~ /^[[:space:]]*\/\*\*/) {
                    in_doc = 1
                    start = i
                    continue
                }
                if (in_doc == 1 && lines[i] ~ /^[[:space:]]*\*\/[[:space:]]*$/) {
                    in_doc = 0
                    returns = 0
                    last = 0
                    for (j = start + 1; j < i; j++) {
                        if (lines[j] ~ /^[[:space:]]*\*[[:space:]]*@return([[:space:]].*)?$/) {
                            returns++
                            last = j
                        }
                    }
                    if (returns >= 2) {
                        delete lines[last]
                        for (j = i - 1; j > start; j--) {
                            if (!(j in lines)) continue
                            if (lines[j] ~ /^[[:space:]]*\*[[:space:]]*$/) delete lines[j]
                            else break
                        }
                    }
                }
            }
            for (i = 1; i <= NR; i++) {
                if (i in lines) print lines[i]
            }
        }
    '
}

# cargo-php always runs with the caller's arguments. The stub is filtered
# through filter_stub() in every mode; a temp file carries the output so a
# cargo-php failure is never masked by the filter (POSIX sh has no
# pipefail).
tmp="$(mktemp "${TMPDIR:-/tmp}/rabbit-rs-stub.XXXXXX")"
trap 'rm -f "${tmp}"' EXIT HUP INT TERM

if [ "${stdout_mode}" -eq 1 ]; then
    # --stdout: capture, filter, print.
    cargo php stubs --manifest "${MANIFEST}" "$@" > "${tmp}"
    filter_stub < "${tmp}"
elif [ -n "${out_path}" ]; then
    # -o <path>: cargo-php writes the file; filter it in place.
    cargo php stubs --manifest "${MANIFEST}" "$@"
    filter_stub < "${out_path}" > "${tmp}"
    mv "${tmp}" "${out_path}"
else
    # Default: reproduce cargo-php's <ext-name>.stubs.php destination (the
    # module name in src/lib.rs is "rabbit_rs") with --stdout forced so the
    # filter always sees the generated stub.
    cargo php stubs --manifest "${MANIFEST}" "$@" --stdout > "${tmp}"
    filter_stub < "${tmp}" > "rabbit_rs.stubs.php"
fi
