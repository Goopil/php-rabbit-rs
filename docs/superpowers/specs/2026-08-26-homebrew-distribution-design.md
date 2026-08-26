# Homebrew Distribution Design

**Date:** 2026-08-26
**Status:** Approved

## Problem

rabbit-rs is a native PHP extension built in Rust and distributed via PIE (PHP Installer for Extensions) with pre-packaged binaries on GitHub Releases. PHP developers on macOS expect to install extensions through Homebrew, the dominant package manager on that platform. The current PIE-based flow requires Composer knowledge and manual binary selection; a Homebrew formula would make installation a two-command operation.

## Scope

**In scope:**
- Homebrew formula that installs the pre-compiled rabbit-rs PHP extension on macOS.
- CI automation to keep the formula in sync with GitHub Releases.
- CI test job that validates the formula end-to-end.

**Out of scope:**
- Linuxbrew (Linux Homebrew support).
- The Laravel queue package (`goopil/rabbit-rs-laravel`) — remains distributed via Composer/Packagist.
- Submission to `homebrew-core` (can be revisited once the project meets notability criteria).
- Thread-safety (ZTS) detection on macOS — Homebrew PHP is NTS-only.

## Architecture

### Repository layout

A dedicated, minimal Homebrew tap repository: `Goopil/homebrew-rabbit-rs`.

```
homebrew-rabbit-rs/
├── Formula/
│   └── rabbit-rs.rb     # The sole formula file
└── README.md            # Install instructions
```

This repository contains only the formula. It is updated automatically by CI in the main `rabbit-rs` repository after each release. The dedicated repo is required because Homebrew's `brew tap goopil/rabbit-rs` convention resolves to `github.com/Goopil/homebrew-rabbit-rs` automatically — no explicit URL needed.

### User experience

```bash
brew tap goopil/rabbit-rs
brew install rabbit-rs
```

Homebrew resolves the tap to `https://github.com/Goopil/homebrew-rabbit-rs`, finds `Formula/rabbit-rs.rb`, and runs the install.

Uninstall:
```bash
brew uninstall rabbit-rs
```

Upgrade:
```bash
brew upgrade rabbit-rs
```

### Formula behavior

The formula is a "binary-only" formula (no compilation from source). At install time it:

1. **Detects the PHP version** by calling `php-config --version` against the Homebrew-installed PHP. The formula declares `depends_on "php"` (unversioned) so it works with whatever major PHP the user has installed via Homebrew (`php@8.4` or `php@8.5`).

2. **Detects the architecture** via `Hardware::CPU.arch` (returns `:arm64` on Apple Silicon).

3. **Constructs the download URL** in the format:
   ```
   https://github.com/Goopil/rabbit-rs/releases/download/v{version}/php_rabbit_rs-v{version}_php{php}-arm64-darwin-nts.zip
   ```
   Only NTS is needed because Homebrew PHP is NTS-only on macOS.

4. **Downloads and stages** the zip using Homebrew's standard download mechanisms.

5. **Installs the shared object** by copying `rabbit_rs.so` (actually a `.dylib` renamed) into the PHP extension directory, resolved via `php-config --extension-dir`.

6. **Creates an INI file** at `#{etc}/php/conf.d/ext-rabbit_rs.ini` (or the Homebrew PHP conf.d path) containing:
   ```ini
   extension=rabbit_rs.so
   ```

7. **On uninstall**, Homebrew's standard cleanup removes installed files. The formula uses `prefix` as the install root so everything is self-contained and removable.

### PHP version compatibility

The formula supports PHP 8.4 and 8.5 — the two versions for which macOS release artifacts exist. If a user has a different PHP version, the formula raises a clear error message indicating which versions are supported and pointing to the PIE alternative.

### Version and checksum management

Because the download URL depends on the PHP version detected at install time, the formula cannot use the standard class-level `url`/`sha256` fields (which are evaluated at formula load time, before any PHP detection can run). Instead, the formula uses Homebrew `resource` blocks — one per supported PHP version — each with its own `url` and `sha256`. The `install` method detects the PHP version and stages the matching resource.

```ruby
resource "php84" do
  url "https://github.com/Goopil/rabbit-rs/releases/download/v0.0.6/php_rabbit_rs-v0.0.6_php8.4-arm64-darwin-nts.zip"
  sha256 "..."
end

resource "php85" do
  url "https://github.com/Goopil/rabbit-rs/releases/download/v0.0.6/php_rabbit_rs-v0.0.6_php8.5-arm64-darwin-nts.zip"
  sha256 "..."
end
```

The formula's top-level `version` field is updated by CI on each release. The `livecheck` block points to the GitHub Releases latest tag so `brew livecheck rabbit-rs` reports available updates.

## CI automation

### Update formula on release

A new job `update-homebrew-formula` is added to `.github/workflows/release.yml`, running after `publish-release`. It:

1. Reads the version from the git tag.
2. Downloads the macOS arm64 zip artifacts for PHP 8.4 and 8.5 from the GitHub Release.
3. Computes sha256 for each.
4. Clones `Goopil/homebrew-rabbit-rs` using a deploy key or `MIRROR_TOKEN`.
5. Updates `Formula/rabbit-rs.rb` — replaces the `version` field and the `url`/`sha256` lines inside each `resource` block (`php84` and `php85`).
6. Commits and pushes to `main` on the tap repo.

### Formula test job

A new job `test-homebrew-formula` (in `release.yml` or a separate workflow) runs on `macos-14`. It:

1. `brew tap` the local tap repo.
2. `brew install --formula Formula/rabbit-rs.rb` (testing the formula directly).
3. Runs `php -m | grep rabbit_rs` to confirm the extension loads.
4. `brew uninstall rabbit-rs` and verifies cleanup.

This job runs on PRs that modify the formula and on release.

## Data flow

```
Tag pushed (v0.0.7)
    │
    ▼
release.yml: build-linux, build-macos
    │
    ▼
verify-release (attestations)
    │
    ▼
publish-release (GitHub Release published)
    │
    ▼
update-homebrew-formula
    │  ├── Download macOS arm64 zips from release
    │  ├── Compute sha256
    │  ├── Clone Goopil/homebrew-rabbit-rs
    │  ├── Update Formula/rabbit-rs.rb (version + sha256)
    │  └── Push to main
    │
    ▼
test-homebrew-formula (macos-14)
    │  ├── brew tap + install
    │  ├── php -m | grep rabbit_rs
    │  └── brew uninstall
    ▼
Done

User:
    brew tap goopil/rabbit-rs
    brew install rabbit-rs
```

## Error handling

| Scenario | Formula behavior |
|----------|-----------------|
| PHP version not 8.4 or 8.5 | `odie` with message: "rabbit-rs requires PHP 8.4 or 8.5. Found {version}. Use PIE for other versions." |
| No PHP installed | `depends_on "php"` ensures Homebrew installs PHP first |
| Architecture not arm64 | `odie` with message: "rabbit-rs Homebrew formula supports Apple Silicon only. Use PIE on Intel Macs." |
| Download fails (404) | Standard Homebrew download error, which surfaces the URL so the user can verify the release exists |
| Extension already loaded | The INI file uses `extension=` which PHP handles gracefully on duplicate loads |

## Testing

- **Formula syntax:** `brew audit --strict Formula/rabbit-rs.rb` in CI.
- **Install test:** `brew install` + `php -m | grep rabbit_rs` on macOS runner.
- **Uninstall test:** `brew uninstall` + verify no leftover files.
- **Upgrade test:** (manual) install old version, `brew upgrade`, verify new version loads.

## Open questions

None at this time. All decisions have been validated through the brainstorming process.

## Future considerations

- **homebrew-core submission:** Once the project meets Homebrew's notability and staleness criteria, the formula can be adapted for submission to homebrew-core. The binary-download approach would need to change to build-from-source for homebrew-core acceptance.
- **Linuxbrew:** If demand exists, the formula can be extended with Linux resource blocks (glibc/musl) matching the existing release matrix.
- **ZTS support:** If Homebrew ever ships ZTS PHP, the formula can detect thread safety via `php-config --php-sapi` or `php -i | grep "Thread Safety"`.
