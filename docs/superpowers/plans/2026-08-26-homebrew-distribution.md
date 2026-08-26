# Homebrew Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable macOS users to install the rabbit-rs PHP extension via Homebrew with a simple `brew tap goopil/rabbit-rs && brew install rabbit-rs`.

**Architecture:** A dedicated minimal Homebrew tap repository (`Goopil/homebrew-rabbit-rs`) containing a single formula that downloads the pre-compiled macOS binary from GitHub Releases, detecting the user's PHP version at install time. The main repo's release CI automatically updates the formula with new version numbers and SHA-256 checksums after each release.

**Tech Stack:** Ruby (Homebrew formula DSL), GitHub Actions CI, Bash (update script), Homebrew's `resource` block pattern for multi-PHP-version support.

## Global Constraints

- The formula must support PHP 8.4 and 8.5 only (the versions with macOS release artifacts).
- macOS arm64 (Apple Silicon) only -- Intel Macs use PIE.
- NTS only -- Homebrew PHP is NTS-only on macOS.
- No compilation from source in the formula -- download pre-built binaries from GitHub Releases.
- The formula must pass `brew audit --strict`.
- The tap repo is `Goopil/homebrew-rabbit-rs` (separate from the main `Goopil/rabbit-rs` repo).
- The CI update job uses the existing `MIRROR_TOKEN` secret (same as the Laravel split job).
- Release artifact naming: `php_rabbit_rs-v{version}_php{php}-arm64-darwin-nts.zip`.

---

## File Structure

### New files in the main repo (`Goopil/rabbit-rs`)

| File | Responsibility |
|------|---------------|
| `scripts/update-homebrew-formula.sh` | Bash script that downloads macOS release artifacts, computes SHA-256, clones the tap repo, and updates the formula. Called by CI. |
| `.github/workflows/homebrew-formula-test.yml` | CI workflow that tests the formula on macOS (install + load + uninstall). Runs on PRs touching formula-related files and on release. |

### Modified files in the main repo

| File | Change |
|------|--------|
| `.github/workflows/release.yml` | Add `update-homebrew-formula` and `test-homebrew-formula` jobs after `publish-release`. |

### New files in the tap repo (`Goopil/homebrew-rabbit-rs`)

| File | Responsibility |
|------|---------------|
| `Formula/rabbit-rs.rb` | The Homebrew formula. |
| `README.md` | Install instructions for users. |

---

## Task 1: Create the Homebrew tap repository and formula

**Files:**
- Create: `Goopil/homebrew-rabbit-rs` repo on GitHub
- Create: `Formula/rabbit-rs.rb` (in the tap repo)
- Create: `README.md` (in the tap repo)

**Interfaces:**
- Produces: a Homebrew formula named `rabbit-rs` with `resource` blocks `php84` and `php85`, a `version` field, and a `livecheck` block. The formula is the contract that Task 2 updates and Task 3 tests.

**Note:** This task creates the initial formula with placeholder SHA-256 values. Task 2 creates the update script that fills them in with real values after each release.

- [ ] **Step 1: Create the tap repo on GitHub**

```bash
gh repo create Goopil/homebrew-rabbit-rs --public \
  --description "Homebrew tap for rabbit-rs -- high-performance RabbitMQ PHP extension powered by Rust"
```

The `MIRROR_TOKEN` secret already used by the `split-laravel` job in `release.yml` must have write access to this repo. If it does not, add the same token as a deploy key or update the token scopes.

- [ ] **Step 2: Create the formula file `Formula/rabbit-rs.rb`**

```ruby
class RabbitRs < Formula
  desc "High-performance RabbitMQ transport for PHP, powered by Rust"
  homepage "https://github.com/Goopil/rabbit-rs"
  license "MIT"
  version "0.0.6"

  depends_on "php"

  # macOS arm64 NTS only. Homebrew PHP is NTS-only on macOS.
  resource "php84" do
    url "https://github.com/Goopil/rabbit-rs/releases/download/v0.0.6/php_rabbit_rs-v0.0.6_php8.4-arm64-darwin-nts.zip"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  resource "php85" do
    url "https://github.com/Goopil/rabbit-rs/releases/download/v0.0.6/php_rabbit_rs-v0.0.6_php8.5-arm64-darwin-nts.zip"
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  end

  livecheck do
    url :stable
    strategy :github_latest
  end

  def install
    php_version = Utils.safe_popen_read(Formula["php"].opt_bin/"php-config", "--version").strip
    php_major_minor = php_version.split(".")[0, 2].join(".")

    supported = ["8.4", "8.5"]
    unless supported.include?(php_major_minor)
      odie "rabbit-rs requires PHP 8.4 or 8.5. Found #{php_version}. Use PIE for other versions."
    end

    if Hardware::CPU.arch != :arm64
      odie "rabbit-rs Homebrew formula supports Apple Silicon only. Use PIE on Intel Macs."
    end

    resource_name = "php" + php_major_minor.tr(".", "")
    resource(resource_name).stage do
      libexec.mkpath
      cp "rabbit_rs.so", libexec/"rabbit_rs.so"
    end
  end

  def post_install
    ext_dir = Utils.safe_popen_read(Formula["php"].opt_bin/"php-config", "--extension-dir").strip

    ini_path = etc/"php/conf.d/ext-rabbit_rs.ini"

    ext_so = Pathname.new(ext_dir)/"rabbit_rs.so"
    ohai "Installing rabbit_rs.so into #{ext_dir}"
    ln_sf libexec/"rabbit_rs.so", ext_so

    ohai "Creating INI file at #{ini_path}"
    ini_path.dirname.mkpath
    File.write(ini_path, "extension=rabbit_rs.so\n")
  end

  def uninstall
    ini_path = etc/"php/conf.d/ext-rabbit_rs.ini"
    ini_path.unlink if ini_path.exist?

    ext_dir = Utils.safe_popen_read(Formula["php"].opt_bin/"php-config", "--extension-dir").strip
    ext_so = Pathname.new(ext_dir)/"rabbit_rs.so"
    ext_so.unlink if ext_so.symlink? && ext_so.exist?
  end

  test do
    assert_match "rabbit_rs", shell_output("#{Formula["php"].opt_bin/"php"} -m")
  end
end
```

- [ ] **Step 3: Create `README.md` in the tap repo**

```markdown
# homebrew-rabbit-rs

Homebrew tap for [rabbit-rs](https://github.com/Goopil/rabbit-rs) -- a high-performance RabbitMQ transport for PHP and Laravel, powered by Rust.

## Installation

```bash
brew tap goopil/rabbit-rs
brew install rabbit-rs
```

## Requirements

- macOS (Apple Silicon)
- PHP 8.4 or 8.5 (installed via Homebrew)

## Uninstall

```bash
brew uninstall rabbit-rs
```

## Alternative installation

For Linux, Intel Macs, or other PHP versions, use [PIE](https://github.com/pie-php):

```bash
pie install rabbit-rs/native
```
```

- [ ] **Step 4: Commit and push the tap repo**

```bash
cd homebrew-rabbit-rs
git add Formula/rabbit-rs.rb README.md
git commit -m "Initial Homebrew formula for rabbit-rs"
git push origin main
```

- [ ] **Step 5: Verify the formula is discoverable**

```bash
brew tap goopil/rabbit-rs
brew info goopil/rabbit-rs/rabbit-rs
```

Expected: brew shows the formula description, version, and dependencies. May warn about SHA-256 mismatch (placeholder values), which is expected before the first release CI run.

---

## Task 2: Create the formula update script

**Files:**
- Create: `scripts/update-homebrew-formula.sh`

**Interfaces:**
- Consumes: `VERSION` via `--version` flag, `MIRROR_TOKEN` env var for git push to the tap repo.
- Produces: an updated `Formula/rabbit-rs.rb` pushed to `Goopil/homebrew-rabbit-rs` with correct `version` and `sha256` values for both PHP 8.4 and 8.5.

- [ ] **Step 1: Create `scripts/update-homebrew-formula.sh`**

```bash
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

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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
  MIRROR_TOKEN      GitHub token with write access to Goopil/homebrew-rabbit-rs
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
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

if ! command -v ruby >/dev/null 2>&1; then
    fail "ruby is required to update the formula"
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
    curl -fsSL -o "${zip_path}" "${download_url}" \
        || fail "failed to download ${download_url}"

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

RUBY_SCRIPT=$(cat <<RUBY
version = "${VERSION}"
sha84 = "${SHAS["8.4"]}"
sha85 = "${SHAS["8.5"]}"
formula_path = "${TAP_DIR}/${FORMULA_PATH}"

content = File.read(formula_path)

content.gsub!(/^  version ".*"/, "  version \"#{version}\"")

content.gsub!(
  /resource "php84" do\n    url ".*"\n    sha256 ".*"/,
  "resource \"php84\" do\n    url \"https://github.com/Goopil/rabbit-rs/releases/download/v#{version}/php_rabbit_rs-v#{version}_php8.4-arm64-darwin-nts.zip\"\n    sha256 \"#{sha84}\""
)

content.gsub!(
  /resource "php85" do\n    url ".*"\n    sha256 ".*"/,
  "resource \"php85\" do\n    url \"https://github.com/Goopil/rabbit-rs/releases/download/v#{version}/php_rabbit_rs-v#{version}_php8.5-arm64-darwin-nts.zip\"\n    sha256 \"#{sha85}\""
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
git commit -m "Update formula to v${VERSION}"
git push origin main 2>&1 | sed 's|https://[^@]*@|https://|g'

ok "formula updated to v${VERSION} and pushed to ${TAP_REPO}"
```

- [ ] **Step 2: Make the script executable**

```bash
chmod +x scripts/update-homebrew-formula.sh
```

- [ ] **Step 3: Test the script syntax**

```bash
bash -n scripts/update-homebrew-formula.sh
```

Expected: no output (syntax OK).

- [ ] **Step 4: Commit**

```bash
git add scripts/update-homebrew-formula.sh
git commit -m "feat: add Homebrew formula update script"
```

---

## Task 3: Add CI jobs to release.yml

**Files:**
- Modify: `.github/workflows/release.yml` (add two jobs after `publish-release`, before `split-laravel`)

**Interfaces:**
- Consumes: `needs.create-release.outputs.version`, `MIRROR_TOKEN` secret, macOS release artifacts from GitHub Releases.
- Produces: updated formula in the tap repo (via `update-homebrew-formula` job), validated formula (via `test-homebrew-formula` job).

- [ ] **Step 1: Add the `update-homebrew-formula` job**

Insert after the `publish-release` job in `.github/workflows/release.yml`, before `split-laravel`:

```yaml
  update-homebrew-formula:
    needs: [create-release, publish-release]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7

      - name: Check prerequisites
        id: check
        env:
          MIRROR_TOKEN: ${{ secrets.MIRROR_TOKEN }}
        run: |
          if [[ -z "${MIRROR_TOKEN}" ]]; then
            echo "::warning::MIRROR_TOKEN secret is not configured -- skipping Homebrew formula update"
            echo "skip=true" >> "$GITHUB_OUTPUT"
          else
            echo "skip=false" >> "$GITHUB_OUTPUT"
          fi

      - name: Update Homebrew formula
        if: steps.check.outputs.skip == 'false'
        env:
          MIRROR_TOKEN: ${{ secrets.MIRROR_TOKEN }}
          VERSION: ${{ needs.create-release.outputs.version }}
        run: ./scripts/update-homebrew-formula.sh --version "${VERSION}"

      - name: Summary
        if: steps.check.outputs.skip == 'true'
        run: echo "::notice::MIRROR_TOKEN not configured -- Homebrew formula update skipped."
```

- [ ] **Step 2: Add the `test-homebrew-formula` job**

Insert after `update-homebrew-formula`:

```yaml
  test-homebrew-formula:
    needs: [update-homebrew-formula]
    runs-on: macos-14
    if: needs.update-homebrew-formula.result == 'success'
    steps:
      - uses: actions/checkout@v7

      - name: Setup PHP 8.4
        uses: shivammathur/setup-php@v2
        with:
          php-version: "8.4"
          coverage: none
          extensions: json

      - name: Tap and install
        run: |
          brew tap goopil/rabbit-rs
          brew install rabbit-rs

      - name: Verify extension loads
        run: |
          php -m | grep -q "rabbit_rs" || { echo "ERROR: rabbit_rs extension not loaded"; exit 1; }
          echo "OK: rabbit_rs loaded, version $(php -r 'echo phpversion("rabbit_rs");')"

      - name: Uninstall and verify cleanup
        run: |
          brew uninstall rabbit-rs
          if php -m 2>/dev/null | grep -q "rabbit_rs"; then
            echo "ERROR: rabbit_rs still loaded after uninstall"
            exit 1
          fi
          echo "OK: rabbit_rs uninstalled cleanly"
```

- [ ] **Step 3: Update `split-laravel` to depend on the new jobs**

Change the `needs` list in the `split-laravel` job from:

```yaml
  split-laravel:
    needs: [publish-release]
```

to:

```yaml
  split-laravel:
    needs: [publish-release, test-homebrew-formula]
```

This ensures the Laravel split only runs after the Homebrew formula is tested.

- [ ] **Step 4: Verify YAML syntax**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"
```

Expected: no error.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "feat: add Homebrew formula update and test jobs to release pipeline"
```

---

## Task 4: Add formula test workflow for PRs

**Files:**
- Create: `.github/workflows/homebrew-formula-test.yml`

**Interfaces:**
- Consumes: the formula from the tap repo `Goopil/homebrew-rabbit-rs`.
- Produces: CI validation that the formula installs and loads correctly on macOS.

- [ ] **Step 1: Create the workflow file**

Create `.github/workflows/homebrew-formula-test.yml`:

```yaml
name: Homebrew Formula Test

on:
  pull_request:
    branches: [main]
    paths:
      - "scripts/update-homebrew-formula.sh"
      - ".github/workflows/homebrew-formula-test.yml"
      - ".github/workflows/release.yml"
  workflow_dispatch:

permissions:
  contents: read

jobs:
  test-formula:
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v7

      - name: Setup PHP 8.4
        uses: shivammathur/setup-php@v2
        with:
          php-version: "8.4"
          coverage: none
          extensions: json

      - name: Tap and install
        run: |
          brew tap goopil/rabbit-rs
          brew install rabbit-rs

      - name: Verify extension loads
        run: |
          php -m | grep -q "rabbit_rs" || { echo "ERROR: rabbit_rs extension not loaded"; exit 1; }
          echo "OK: rabbit_rs loaded, version $(php -r 'echo phpversion("rabbit_rs");')"

      - name: Uninstall and verify cleanup
        run: |
          brew uninstall rabbit-rs
          if php -m 2>/dev/null | grep -q "rabbit_rs"; then
            echo "ERROR: rabbit_rs still loaded after uninstall"
            exit 1
          fi
          echo "OK: rabbit_rs uninstalled cleanly"
```

- [ ] **Step 2: Verify YAML syntax**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/homebrew-formula-test.yml'))"
```

Expected: no error.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/homebrew-formula-test.yml
git commit -m "feat: add Homebrew formula PR test workflow"
```

---

## Task 5: Documentation update

**Files:**
- Modify: `README.md` (root of main repo)

**Interfaces:**
- Produces: updated README with Homebrew installation instructions.

- [ ] **Step 1: Read the current README**

Run: `cat README.md`

Find the installation section and note its structure and formatting conventions.

- [ ] **Step 2: Add Homebrew installation instructions**

Add a Homebrew section to the installation instructions, following the existing README style. The section should include:

```markdown
### Homebrew (macOS Apple Silicon)

```bash
brew tap goopil/rabbit-rs
brew install rabbit-rs
```

Requires PHP 8.4 or 8.5 installed via Homebrew.
```

Place this section alongside the existing PIE installation instructions, not replacing them.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add Homebrew installation instructions to README"
```

---

## Task 6: First release verification

**Files:**
- No new files. This task verifies the end-to-end flow.

- [ ] **Step 1: Verify the tap repo is accessible**

```bash
brew tap goopil/rabbit-rs
brew info goopil/rabbit-rs/rabbit-rs
```

Expected: formula info displayed with version matching the latest release.

- [ ] **Step 2: Install via Homebrew**

```bash
brew install rabbit-rs
```

Expected: formula downloads the macOS binary, installs it, and creates the INI file.

- [ ] **Step 3: Verify the extension loads**

```bash
php -m | grep rabbit_rs
php -r 'echo phpversion("rabbit_rs") . "\n";'
```

Expected: `rabbit_rs` appears in the module list and the version matches the installed formula version.

- [ ] **Step 4: Uninstall and verify cleanup**

```bash
brew uninstall rabbit-rs
php -m | grep -c rabbit_rs || true
```

Expected: rabbit_rs no longer in module list, no leftover files.

- [ ] **Step 5: Test upgrade path**

```bash
brew install rabbit-rs@0.0.5  # or whatever previous version is available
brew upgrade rabbit-rs
php -m | grep rabbit_rs
```

Expected: upgrade succeeds and the new version loads correctly.
