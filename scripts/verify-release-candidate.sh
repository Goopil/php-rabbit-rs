#!/usr/bin/env bash
set -uo pipefail

# verify-release-candidate.sh — RC pipeline orchestrator (issue #177).
#
# Runs the full release-candidate tier list in order, prints PASS/FAIL/SKIP
# per tier with a final summary, and exits non-zero on any blocking failure.
# All tier output is teed into an evidence directory (uploaded as workflow
# artifacts by .github/workflows/release-candidate.yml).
#
# Tiers (blocking unless noted):
#   1. scripts/check.sh                      — fast gate (fmt, clippy, tests, composer, deny)
#   2. scripts/test-extension.sh             — extension Pest + PHPT
#   3. scripts/test-laravel.sh               — Laravel Unit + Feature (no extension)
#   4. scripts/test-integration.sh --with-tls — Rust + Laravel integration on the TLS lab
#   5. scripts/test-fpm.sh                   — broker-backed FPM certification (external-lab mode;
#                                              the orchestrator brings the lab up before this tier
#                                              and shares it with tiers 6, 8, 9)
#   6. scripts/test-octane-runtime.sh        — one run per Octane server (roadrunner, frankenphp,
#                                              swoole, openswoole); harness exit 2 ("server not
#                                              available on this machine") counts as SKIP with
#                                              the reason, not FAIL — the certification evidence
#                                              for unavailable servers comes from the nightly
#                                              matrix, which provisions each server in its own job
#   7. scripts/validate-distribution.sh      — packaging checks (full only when release/ has archives)
#   8. scripts/amqp-smoke.sh                 — per release artifact in release/ that matches this
#                                              platform (php version + arch + libc); SKIP with an
#                                              explicit note when release/ is empty or nothing matches
#   9. Budget check (ADVISORY)               — fresh rebench driver-bench run, compared against a
#                                              baseline produced by the same run (self-comparison,
#                                              because the committed single-machine baseline is not
#                                              portable across runners). Losses/duplicates/ok are
#                                              blocking; ratio thresholds are advisory. Requires the
#                                              macOS release dylib (rebench-driver-bench.sh hardcodes
#                                              target/release/librabbit_rs_php.dylib); SKIPs elsewhere.
#
# Usage:
#   ./scripts/verify-release-candidate.sh [--dry-run] [--evidence-dir <dir>] [-h]
#
# Exit codes: 0 all blocking tiers passed; 1 a blocking tier failed or (with
# --dry-run) prerequisites are missing; 2 usage error.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}" || exit 1

DRY_RUN=false
EVIDENCE_DIR=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true ;;
        --evidence-dir) [[ $# -ge 2 ]] || { echo "ERROR: --evidence-dir needs a value" >&2; exit 2; }; EVIDENCE_DIR="$2"; shift ;;
        -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "ERROR: unknown argument '$1' (see --help)" >&2; exit 2 ;;
    esac
    shift
done

PHP_BIN="${PHP_BIN:-php}"

# ---------------------------------------------------------------------------
# Tier bookkeeping.
# ---------------------------------------------------------------------------

TIER_IDS=()
TIER_NAMES=()
TIER_STATUS=()
TIER_SECONDS=()
TIER_NOTES=()

record_tier() {
    TIER_IDS+=("$1"); TIER_NAMES+=("$2"); TIER_STATUS+=("$3"); TIER_SECONDS+=("$4"); TIER_NOTES+=("$5")
    echo "TIER $1 [$3] $2 ${5:+— $5}"
}

# Runs one tier command, teeing output into the evidence log. $1 = tier id,
# $2 = tier name, $3 = log name, remaining = command words.
run_tier() {
    local id="$1" name="$2" log="$3"
    shift 3
    local log_path="${EVIDENCE_DIR}/${log}"
    echo ""
    echo "=== Tier ${id}: ${name} ==="
    local started=$SECONDS
    "$@" 2>&1 | tee "${log_path}"
    local rc=${PIPESTATUS[0]}
    record_tier "${id}" "${name}" "$([[ ${rc} -eq 0 ]] && echo PASS || echo FAIL)" "$((SECONDS - started))" ""
    return "${rc}"
}

skip_tier() {
    local id="$1" name="$2" note="$3"
    record_tier "${id}" "${name}" "SKIP" "0" "${note}"
}

tier_name_by_id() {
    case "$1" in
        1) echo "scripts/check.sh (fast gate)" ;;
        2) echo "scripts/test-extension.sh (Pest + PHPT)" ;;
        3) echo "scripts/test-laravel.sh (Unit + Feature)" ;;
        4) echo "scripts/test-integration.sh --with-tls" ;;
        5) echo "scripts/test-fpm.sh (external lab)" ;;
        6) echo "scripts/test-octane-runtime.sh x4" ;;
        7) echo "scripts/validate-distribution.sh" ;;
        8) echo "scripts/amqp-smoke.sh (release artifacts)" ;;
        9) echo "budget check (rebench + self-baseline)" ;;
        *) echo "tier $1" ;;
    esac
}

# Marks the run aborted and SKIPs every remaining tier id passed as arguments.
abort_remaining() {
    local failed_id="$1"
    shift
    ABORT=true
    for pending in "$@"; do
        skip_tier "${pending}" "$(tier_name_by_id "${pending}")" "aborted after tier ${failed_id} failed"
    done
}

# ---------------------------------------------------------------------------
# Prerequisites.
# ---------------------------------------------------------------------------

missing_hard=()
missing_soft=()

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || missing_hard+=("$1")
}

_ext_suffix() {
    case "$(uname -s)" in
        Darwin) echo "dylib" ;;
        Linux) echo "so" ;;
        *) echo "unsupported" ;;
    esac
}

EXT_SUFFIX="$(_ext_suffix)"
DEBUG_ARTIFACT="${ROOT}/target/debug/librabbit_rs_php.${EXT_SUFFIX}"
RELEASE_ARTIFACT="${ROOT}/target/release/librabbit_rs_php.${EXT_SUFFIX}"

php_major_minor() {
    "${PHP_BIN}" -r 'echo PHP_MAJOR_VERSION . "." . PHP_MINOR_VERSION;' 2>/dev/null || echo "unknown"
}

# Populates missing_hard / missing_soft. Prints nothing; callers format output.
scan_prerequisites() {
    need_cmd cargo
    need_cmd "${PHP_BIN}"
    need_cmd php-config
    need_cmd composer
    need_cmd docker
    need_cmd jq
    need_cmd curl
    need_cmd git
    if [[ "${EXT_SUFFIX}" == "unsupported" ]]; then
        missing_hard+=("supported OS (darwin/linux); got $(uname -s)")
    fi
    if [[ ${#missing_hard[@]} -eq 0 ]]; then
        local php_mm
        php_mm="$(php_major_minor)"
        if [[ "${php_mm}" == "unknown" || "$(awk -v v="${php_mm}" 'BEGIN{print (v < 8.4)}')" == "1" ]]; then
            missing_hard+=("php >= 8.4 (found ${php_mm})")
        fi
        if ! docker ps >/dev/null 2>&1; then
            missing_hard+=("docker daemon running")
        fi
    fi
    # Soft prerequisites: the tier scripts install or build these on demand.
    [[ -d "${ROOT}/packages/laravel-queue/vendor" ]] || missing_soft+=("packages/laravel-queue/vendor (composer install)")
    [[ -d "${ROOT}/crates/rabbit-rs-php/vendor" ]] || missing_soft+=("crates/rabbit-rs-php/vendor (composer install)")
    [[ -d "${ROOT}/benchmarks/driver-bench/vendor" ]] || missing_soft+=("benchmarks/driver-bench/vendor (composer install)")
    [[ -f "${DEBUG_ARTIFACT}" ]] || missing_soft+=("target/debug/librabbit_rs_php.${EXT_SUFFIX} (auto-built by the tiers)")
    [[ -f "${RELEASE_ARTIFACT}" ]] || missing_soft+=("target/release/librabbit_rs_php.${EXT_SUFFIX} (tier 9; built on demand on macOS)")
    # Hard: the SAN-negative TLS case resolves wrong.internal; without the
    # mapping tier 4 deterministically fails ~15 min in, so gate early.
    if ! grep -q 'wrong\.internal' /etc/hosts 2>/dev/null; then
        missing_hard+=("/etc/hosts mapping for the SAN-negative TLS case (run: sudo sh -c 'echo \"127.0.0.1 wrong.internal\" >> /etc/hosts'; CI adds the same line)")
    fi
}

print_prerequisites() {
    echo "Prerequisites:"
    local item
    for item in cargo "${PHP_BIN}" php-config composer docker jq curl git; do
        if command -v "${item}" >/dev/null 2>&1; then
            echo "  ok      ${item}"
        else
            echo "  MISSING ${item}"
        fi
    done
    if docker ps >/dev/null 2>&1; then
        echo "  ok      docker daemon"
    else
        echo "  MISSING docker daemon (docker ps failed)"
    fi
    echo "  info    php ${PHP_BIN} version: $(php_major_minor)"
    if grep -q 'wrong\.internal' /etc/hosts 2>/dev/null; then
        echo "  ok      /etc/hosts wrong.internal mapping (SAN-negative TLS case)"
    else
        echo "  MISSING /etc/hosts wrong.internal mapping (SAN-negative TLS case)"
    fi
    for item in "${missing_soft[@]:-}"; do
        [[ -z "${item}" ]] && continue
        echo "  MISSING ${item}"
    done
}

# ---------------------------------------------------------------------------
# RabbitMQ lab management (tiers 5, 6, 8, 9 share one lab).
# ---------------------------------------------------------------------------

ORCH_STARTED_LAB=false

lab_is_ready() {
    (cd "${ROOT}" && ./scripts/lab-ready.sh >/dev/null 2>&1)
}

ensure_lab() {
    if lab_is_ready; then
        echo "Lab is already running (reused)."
        return 0
    fi
    echo "=== Starting RabbitMQ lab (with-plugin) ==="
    if ! (cd "${ROOT}" && ./scripts/lab-up.sh with-plugin); then
        echo "ERROR: ./scripts/lab-up.sh with-plugin failed" >&2
        return 1
    fi
    ORCH_STARTED_LAB=true
    local i
    for i in $(seq 1 180); do
        if lab_is_ready; then
            echo "Lab is ready."
            return 0
        fi
        sleep 1
    done
    echo "ERROR: lab did not become ready within 180s" >&2
    return 1
}

cleanup() {
    if [[ "${ORCH_STARTED_LAB}" == true ]]; then
        echo ""
        echo "=== Stopping RabbitMQ lab (started by the orchestrator) ==="
        (cd "${ROOT}" && ./scripts/lab-down.sh) || true
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Tier 9 helpers: fresh baseline generation + integrity scan.
# ---------------------------------------------------------------------------

# Emits a self-baseline (medians of this run's own driver-bench results) for
# the goopil scenarios. Third-party driver cells stay out of 'baselines' to
# mirror the committed reference-machine.json contract (context only).
write_fresh_baseline() {
    local results_dir="$1" out="$2"
    "${PHP_BIN}" -r '
        $dir = $argv[1];
        $out = $argv[2];
        $groups = [];
        foreach (glob($dir."/*.json") ?: [] as $file) {
            $d = json_decode((string) file_get_contents($file), true);
            if (!is_array($d) || ($d["benchmark"] ?? null) !== "driver-bench") {
                continue;
            }
            $connection = (string) ($d["connection"] ?? "");
            $mode = (string) ($d["mode"] ?? "");
            if ($connection !== "rabbit-rs") {
                continue;
            }
            $safety = $d["config"]["rabbit_rs_global"]["safety"] ?? $d["config"]["safety"] ?? null;
            $scenario = $mode === "dispatch" ? "goopil-dispatch".(in_array($safety, ["blind", "safe"], true) ? "-".$safety : "")
                : ($mode === "worker" ? "goopil-worker" : null);
            if ($scenario === null) {
                continue;
            }
            if (is_numeric($d["avg_rate_ops_s"] ?? null)) {
                $groups[$scenario]["throughput_ops_s"][] = (float) $d["avg_rate_ops_s"];
            }
            if (is_numeric($d["latency_ms"]["p99"] ?? null)) {
                $groups[$scenario]["p99_ms"][] = (float) $d["latency_ms"]["p99"];
            }
        }
        $median = function (array $values): ?float {
            if ($values === []) { return null; }
            sort($values);
            $mid = intdiv(count($values), 2);
            return count($values) % 2 === 1 ? $values[$mid] : ($values[$mid - 1] + $values[$mid]) / 2.0;
        };
        $baselines = [];
        foreach ($groups as $scenario => $metrics) {
            $baselines[$scenario] = [
                "throughput_ops_s" => $median($metrics["throughput_ops_s"] ?? []),
                "p99_ms" => $median($metrics["p99_ms"] ?? []),
            ];
        }
        if ($baselines === []) {
            fwrite(STDERR, "no rabbit-rs driver-bench results found in {$dir}\n");
            exit(1);
        }
        $document = [
            "schema" => "rabbit-rs-rc-self-baseline-v1",
            "meta" => [
                "recorded" => gmdate("Y-m-d"),
                "note" => "Self-baseline produced by the same RC run it gates; single-machine medians, not portable across runners. Ratio comparisons are advisory (verify-release-candidate.sh tier 9).",
            ],
            "thresholds" => ["throughput_min_ratio" => 0.8, "p99_max_ratio" => 1.5],
            "baselines" => $baselines,
        ];
        file_put_contents($out, json_encode($document, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES)."\n");
    ' "${results_dir}" "${out}"
}

# Blocking integrity scan over the fresh result JSONs: ok must be true,
# losses and (when reported) duplicates must be 0. Mirrors the checker's
# always-blocking rows; a null metric means "not measured" and stays n/a.
scan_budget_integrity() {
    local results_dir="$1"
    "${PHP_BIN}" -r '
        $failures = 0;
        foreach (glob($argv[1]."/*.json") ?: [] as $file) {
            $d = json_decode((string) file_get_contents($file), true);
            if (!is_array($d) || ($d["benchmark"] ?? null) !== "driver-bench") {
                continue;
            }
            $name = basename($file);
            if (isset($d["ok"]) && $d["ok"] !== true) {
                echo "INTEGRITY FAIL {$name}: ok is false\n";
                $failures++;
            }
            foreach (["losses", "duplicates"] as $key) {
                if (isset($d[$key]) && is_numeric($d[$key]) && (int) $d[$key] !== 0) {
                    echo "INTEGRITY FAIL {$name}: {$key} = {$d[$key]}\n";
                    $failures++;
                }
            }
        }
        exit($failures > 0 ? 1 : 0);
    ' "${results_dir}"
}

# ---------------------------------------------------------------------------
# Dry-run.
# ---------------------------------------------------------------------------

if [[ "${DRY_RUN}" == true ]]; then
    scan_prerequisites
    print_prerequisites
    echo ""
    echo "RC tier plan (estimates vary with machine and cache state):"
    cat <<'PLAN'
  1. scripts/check.sh                        ~10-25 min   BLOCKING
  2. scripts/test-extension.sh               ~5-10 min    BLOCKING
  3. scripts/test-laravel.sh                 ~2-5 min     BLOCKING
  4. scripts/test-integration.sh --with-tls  ~10-20 min   BLOCKING
  5. scripts/test-fpm.sh (external lab)      ~3-5 min     BLOCKING
  6. scripts/test-octane-runtime.sh x4       ~40-80 min   BLOCKING (harness "not available" = SKIP)
  7. scripts/validate-distribution.sh        ~1-2 min     BLOCKING
  8. scripts/amqp-smoke.sh per matching      ~2-5 min     BLOCKING (SKIP with note when release/ has
     artifact in release/                                   no matching artifact)
  9. rebench + check-budgets (self-baseline) ~10-20 min   ADVISORY (losses/duplicates blocking;
                                                            macOS release dylib required, SKIPs on Linux)
PLAN
    echo ""
    echo "The RabbitMQ lab is started before tier 5 and shared by tiers 5, 6, 8, 9."
    if compgen -G "${ROOT}/release/*.zip" >/dev/null; then
        echo "release/ contains $(compgen -G "${ROOT}/release/*.zip" | wc -l | tr -d ' ') archive(s); tier 8 will smoke the platform-matching ones."
    else
        echo "release/ has no archives: tier 8 will be skipped with an explicit note."
    fi
    echo ""
    if [[ ${#missing_hard[@]} -gt 0 ]]; then
        echo "DRY-RUN: FAIL — missing hard prerequisites: ${missing_hard[*]}"
        echo "Install the missing prerequisites and re-run."
        exit 1
    fi
    if [[ ${#missing_soft[@]} -gt 0 ]]; then
        echo "DRY-RUN: FAIL — missing (auto-fixable by the tiers, but the dry-run gate is strict):"
        for item in "${missing_soft[@]}"; do
            echo "  ${item}"
        done
        echo "Run composer install / build the extension, or accept that the tiers fix these in place."
        exit 1
    fi
    echo "DRY-RUN: PASS — all prerequisites present."
    exit 0
fi

# ---------------------------------------------------------------------------
# Real run.
# ---------------------------------------------------------------------------

EVIDENCE_DIR="${EVIDENCE_DIR:-${ROOT}/target/rc-evidence/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "${EVIDENCE_DIR}"
echo "RC evidence directory: ${EVIDENCE_DIR}"

scan_prerequisites
if [[ ${#missing_hard[@]} -gt 0 ]]; then
    echo "ERROR: missing hard prerequisites: ${missing_hard[*]}" >&2
    exit 1
fi
for item in "${missing_soft[@]:-}"; do
    [[ -n "${item}" ]] && echo "note: missing (tiers will fix in place): ${item}"
done

ABORT=false

# Tier 1: fast gate -----------------------------------------------------------
if [[ "${ABORT}" == false ]]; then
    run_tier 1 "$(tier_name_by_id 1)" "tier-01-check.log" ./scripts/check.sh || abort_remaining 1 2 3 4 5 6 7 8 9
fi

# Tier 2: extension Pest + PHPT ------------------------------------------------
if [[ "${ABORT}" == false ]]; then
    run_tier 2 "$(tier_name_by_id 2)" "tier-02-extension.log" ./scripts/test-extension.sh || abort_remaining 2 3 4 5 6 7 8 9
fi

# Tier 3: Laravel Unit + Feature ------------------------------------------------
if [[ "${ABORT}" == false ]]; then
    run_tier 3 "$(tier_name_by_id 3)" "tier-03-laravel.log" ./scripts/test-laravel.sh || abort_remaining 3 4 5 6 7 8 9
fi

# Tier 4: integration + TLS ------------------------------------------------------
if [[ "${ABORT}" == false ]]; then
    run_tier 4 "$(tier_name_by_id 4)" "tier-04-integration-tls.log" ./scripts/test-integration.sh --with-tls || abort_remaining 4 5 6 7 8 9
fi

# Tier 5: FPM certification ------------------------------------------------------
# The lab is brought up here and shared with tiers 6, 8, 9 (one boot instead
# of five); test-fpm.sh runs in external-lab mode against it.
if [[ "${ABORT}" == false ]]; then
    if ensure_lab; then
        run_tier 5 "$(tier_name_by_id 5)" "tier-05-fpm.log" env RABBIT_RS_FPM_EXTERNAL_LAB=1 ./scripts/test-fpm.sh || abort_remaining 5 6 7 8 9
    else
        record_tier 5 "$(tier_name_by_id 5)" FAIL 0 "lab did not start"
        abort_remaining 5 6 7 8 9
    fi
fi

# Tier 6: Octane runtime certification, four servers ------------------------------
OCTANE_SERVERS=(roadrunner frankenphp swoole openswoole)
if [[ "${ABORT}" == false ]]; then
    echo ""
    echo "=== Tier 6: scripts/test-octane-runtime.sh (4 servers) ==="
    octane_failures=0
    for server in "${OCTANE_SERVERS[@]}"; do
        echo "--- Octane server: ${server} ---"
        ./scripts/test-octane-runtime.sh --server="${server}" --reuse-lab 2>&1 | tee "${EVIDENCE_DIR}/tier-06-octane-${server}.log"
        rc=${PIPESTATUS[0]}
        case "${rc}" in
            0) record_tier 6 "octane:${server}" PASS 0 "" ;;
            2) skip_tier 6 "octane:${server}" "harness exit 2 — server not available on this machine (the nightly matrix provides this server's evidence)" ;;
            *) record_tier 6 "octane:${server}" FAIL 0 "exit ${rc}"; octane_failures=$((octane_failures + 1)) ;;
        esac
    done
    if [[ ${octane_failures} -gt 0 ]]; then
        abort_remaining 6 7 8 9
    fi
fi

# Tier 7: distribution checks -----------------------------------------------------
if [[ "${ABORT}" == false ]]; then
    run_tier 7 "$(tier_name_by_id 7)" "tier-07-distribution.log" ./scripts/validate-distribution.sh || abort_remaining 7 8 9
fi

# Tier 8: AMQP smoke over matching release artifacts --------------------------------
# Smoke every release artifact that matches this platform: php version,
# architecture, OS/libc. Non-matching archives are named in the skip note —
# they are covered by the functional-matrix workflow cells instead.
if [[ "${ABORT}" == false ]]; then
    if ensure_lab; then
        shopt -s nullglob
        release_zips=("${ROOT}"/release/*.zip)
        shopt -u nullglob
        if [[ ${#release_zips[@]} -eq 0 ]]; then
            skip_tier 8 "$(tier_name_by_id 8)" "release/ contains no archives — run the release workflow and assemble release/ to exercise this tier"
        else
            php_mm="$(php_major_minor)"
            arch="$(uname -m | sed 's/aarch64/arm64/')"
            os="$(uname -s | tr '[:upper:]' '[:lower:]')"
            case "${os}:${EXT_SUFFIX}" in
                linux:so)
                    libc="glibc"
                    if ldd --version 2>/dev/null | grep -qi musl; then libc="musl"; fi
                    match_pattern="php${php_mm}-${arch}-linux-${libc}-nts.zip"
                    ;;
                darwin:dylib)
                    match_pattern="php${php_mm}-${arch}-darwin-nts.zip"
                    ;;
                *)
                    match_pattern="php${php_mm}-${arch}-unknown-platform"
                    ;;
            esac
            matched=()
            others=()
            for zip in "${release_zips[@]}"; do
                base="$(basename "${zip}")"
                if [[ "${base}" == php_rabbit_rs-*_"${match_pattern}" ]]; then
                    matched+=("${zip}")
                else
                    others+=("${base}")
                fi
            done
            if [[ ${#matched[@]} -eq 0 ]]; then
                skip_tier 8 "$(tier_name_by_id 8)" "no artifact matches this platform (${match_pattern}); present: ${others[*]} — non-matching cells are covered by the functional-matrix workflow"
            else
                smoke_failures=0
                for zip in "${matched[@]}"; do
                    base="$(basename "${zip}" .zip)"
                    extract_dir="${EVIDENCE_DIR}/smoke/${base}"
                    mkdir -p "${extract_dir}"
                    if ! unzip -o -q "${zip}" -d "${extract_dir}"; then
                        record_tier 8 "smoke:${base}" FAIL 0 "unzip failed"
                        smoke_failures=$((smoke_failures + 1))
                        continue
                    fi
                    ext_so="$(find "${extract_dir}" -maxdepth 2 -type f \( -name 'rabbit_rs.so' -o -name 'librabbit_rs_php.*' \) -print -quit)"
                    if [[ -z "${ext_so}" ]]; then
                        record_tier 8 "smoke:${base}" FAIL 0 "no extension binary inside the archive"
                        smoke_failures=$((smoke_failures + 1))
                        continue
                    fi
                    ./scripts/amqp-smoke.sh --php "${PHP_BIN}" --ext "${ext_so}" 2>&1 | tee "${EVIDENCE_DIR}/tier-08-smoke-${base}.log"
                    rc=${PIPESTATUS[0]}
                    if [[ ${rc} -eq 0 ]]; then
                        record_tier 8 "smoke:${base}" PASS 0 ""
                    else
                        record_tier 8 "smoke:${base}" FAIL 0 "amqp-smoke exit ${rc}"
                        smoke_failures=$((smoke_failures + 1))
                    fi
                done
                if [[ ${smoke_failures} -gt 0 ]]; then
                    abort_remaining 8 9
                fi
            fi
        fi
    else
        record_tier 8 "$(tier_name_by_id 8)" FAIL 0 "lab did not start"
        abort_remaining 8 9
    fi
fi

# Tier 9 (advisory thresholds; losses/duplicates blocking) ---------------------------
# Self-comparison by design: the committed baseline is single-machine, so the
# RC run re-baselines itself from its own fresh rebench output. The checker's
# ratio rows are therefore advisory; the blocking verdict comes from the
# integrity scan (ok / losses / duplicates) over the same results.
if [[ "${ABORT}" == false ]]; then
    tier9_skip_note=""
    if [[ "${EXT_SUFFIX}" != "dylib" ]]; then
        tier9_skip_note="rebench-driver-bench.sh hardcodes target/release/librabbit_rs_php.dylib (macOS); the budget comparison is produced on a macOS RC run instead"
    elif [[ ! -f "${RELEASE_ARTIFACT}" ]]; then
        echo ""
        echo "=== Tier 9: building the release extension for the budget run ==="
        if ! cargo build --release --manifest-path crates/rabbit-rs-php/Cargo.toml || [[ ! -f "${RELEASE_ARTIFACT}" ]]; then
            tier9_skip_note="release dylib could not be built"
        fi
    fi
    if [[ -n "${tier9_skip_note}" ]]; then
        skip_tier 9 "$(tier_name_by_id 9)" "${tier9_skip_note}"
    elif ensure_lab; then
        results_dir="${EVIDENCE_DIR}/budget/results"
        baseline_json="${EVIDENCE_DIR}/budget/baseline-self.json"
        mkdir -p "${EVIDENCE_DIR}/budget"
        echo ""
        echo "=== Tier 9: fresh rebench run (self-baseline budget check) ==="
        if ./scripts/rebench-driver-bench.sh "${results_dir}" 2>&1 | tee "${EVIDENCE_DIR}/tier-09-rebench.log"; then
            if write_fresh_baseline "${results_dir}" "${baseline_json}"; then
                echo "Self-baseline written: ${baseline_json}"
                if "${PHP_BIN}" benchmarks/baselines/check-budgets.php "${results_dir}" "--baseline=${baseline_json}" 2>&1 | tee "${EVIDENCE_DIR}/tier-09-check-budgets.log"; then
                    checker_rc=0
                else
                    checker_rc=1
                fi
                if [[ ${checker_rc} -ne 0 ]]; then
                    echo "ADVISORY: check-budgets.php exited ${checker_rc} — ratio thresholds are advisory in the self-comparison (single-machine baselines are not portable); only losses/duplicates/ok block."
                fi
                if scan_budget_integrity "${results_dir}" | tee "${EVIDENCE_DIR}/tier-09-integrity.log"; then
                    record_tier 9 "$(tier_name_by_id 9)" PASS 0 "integrity clean; ratios advisory (self-comparison)"
                else
                    record_tier 9 "$(tier_name_by_id 9)" FAIL 0 "losses/duplicates/ok integrity failure — blocking"
                    ABORT=true
                fi
            else
                record_tier 9 "$(tier_name_by_id 9)" FAIL 0 "could not derive the self-baseline from the fresh results — blocking"
                ABORT=true
            fi
        else
            record_tier 9 "$(tier_name_by_id 9)" FAIL 0 "rebench run failed — integrity could not be measured, blocking"
            ABORT=true
        fi
    else
        record_tier 9 "$(tier_name_by_id 9)" FAIL 0 "lab did not start — blocking"
        ABORT=true
    fi
fi

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------

echo ""
echo "=== RC summary (evidence: ${EVIDENCE_DIR}) ==="
blocking_failures=0
for i in "${!TIER_IDS[@]}"; do
    printf '  TIER %s  %-8s %-42s %6ss  %s\n' \
        "${TIER_IDS[$i]}" "[${TIER_STATUS[$i]}]" "${TIER_NAMES[$i]}" "${TIER_SECONDS[$i]}" "${TIER_NOTES[$i]}"
    if [[ "${TIER_STATUS[$i]}" == "FAIL" ]]; then
        blocking_failures=$((blocking_failures + 1))
    fi
done
echo ""
if [[ ${blocking_failures} -gt 0 ]]; then
    echo "RC VERDICT: FAIL — ${blocking_failures} blocking tier failure(s)."
    exit 1
fi
echo "RC VERDICT: PASS — all blocking tiers green (tier 9 thresholds advisory)."
