# Installation

This guide covers installing the Rabbit RS native extension and the Laravel queue driver.

## Prerequisites

- PHP 8.4 or 8.5 (**NTS only** — ZTS is not supported in V1, see [Thread safety](#thread-safety))
- Linux x86_64 or ARM64 (glibc or musl)
- RabbitMQ 4.2.9 or newer (reachable from your PHP process — the CI lab runs 4.2.9)
- [PIE](https://github.com/php/pie) 1.4.10+ for extension installation (the version the release pipeline validates against)
- [Composer](https://getcomposer.org) for the Laravel queue driver

> **macOS** (Apple Silicon) is supported through the Homebrew tap or a manual release download; Windows is not supported in V1. macOS installs are validated best-effort — see [How pre-packaged binaries work](#how-pre-packaged-binaries-work).

### Thread safety

V1 ships **NTS binaries only**. PIE will not match a ZTS PHP installation (`composer.json` declares `"support-zts": false`). This is deliberate: the extension keeps a process-global runtime and connection registry, and TSRM per-thread isolation is not implemented in V1, so ZTS binaries would share that registry across PHP threads without synchronization. The previous advisory ZTS CI job (`continue-on-error`) only proved that a ZTS binary loads, not that it is safe under real concurrency. ZTS support is planned for V2 with per-thread isolation, a blocking ZTS CI job, and real concurrency tests — tracked in [`docs/plans/ROADMAP.md`](plans/ROADMAP.md) (Parked — ZTS).

## Step 1 — Install the native extension

```bash
pie install goopil/rabbit-rs-native
```

PIE selects the correct pre-compiled binary for your environment:

- PHP version (8.4 or 8.5)
- Architecture (x86_64 or arm64)
- libc (glibc or musl)
- Thread safety (NTS only in V1)

It copies the shared object (`rabbit_rs.so`) to your PHP extension directory and enables it in the active PHP configuration.

### Verify installation

```bash
php --ri rabbit_rs
```

Expected output:

```
rabbit_rs

Rabbit RS - High-performance RabbitMQ transport for PHP and Laravel, powered by Rust
Version => 0.1.2
...
```

### Dockerfile usage

Use PIE in a multi-stage Dockerfile. No dedicated Rabbit RS image is needed:

```dockerfile
FROM php:8.4-cli AS base

# Install PIE
RUN curl -L https://github.com/php/pie/releases/latest/download/pie.phar -o /usr/local/bin/pie \
    && chmod +x /usr/local/bin/pie

# Install the extension
RUN pie install goopil/rabbit-rs-native

# Verify
RUN php --ri rabbit_rs

# Install Composer and the Laravel queue driver
COPY --from=composer:latest /usr/bin/composer /usr/bin/composer
RUN composer require goopil/rabbit-rs-laravel

# ... your application
```

For a complete Dockerfile example, see [examples/laravel/Dockerfile](../examples/laravel/Dockerfile).

## Step 2 — Install the Laravel queue driver

```bash
composer require goopil/rabbit-rs-laravel
```

Composer installs the PHP package and verifies that `ext-rabbit_rs` is loaded. It does **not** install or modify system PHP binaries — that is PIE's job.

The package auto-discovers the service provider in Laravel 12 and 13. If you disabled auto-discovery, register it manually:

```php
// config/app.php
'providers' => [
    // ...
    Goopil\RabbitRs\Laravel\RabbitMqServiceProvider::class,
],
```

## Step 3 — Publish the configuration

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

This creates `config/rabbit-rs.php` with sensible defaults. See [Configuration](configuration.md) for the full reference.

## Step 4 — Verify the installation

```bash
php artisan rabbit-rs:status
```

This displays connection state, pool metrics, and consumer stats. For machine-readable output:

```bash
php artisan rabbit-rs:status --format=json
```

## Local compilation with Cargo

For contributors or environments without PIE:

```bash
# Clone the repository
git clone https://github.com/Goopil/php-rabbit-rs.git
cd php-rabbit-rs

# Build the extension in release mode
cargo build --release -p rabbit-rs-php

# Install into the current PHP
./scripts/install.sh --release
```

The `install.sh` script wraps `cargo php install` with the correct manifest path (the workspace root `Cargo.toml` is workspace-only, so `cargo-php` needs the package manifest at `crates/rabbit-rs-php/Cargo.toml`).

### Requirements for local compilation

- Rust 1.96.0 (pinned in `rust-toolchain.toml`)
- `cargo-php` (install with `cargo install cargo-php`)
- PHP 8.4 or 8.5 with development headers
- `libssl-dev` (or `openssl-devel` / `openssl-dev` depending on your distro)

## Why Composer doesn't modify system PHP

The native extension is a binary shared object (`rabbit_rs.so`) that must be compiled for your specific PHP version, architecture, libc, and thread-safety mode. Composer is a PHP dependency manager — it handles PHP source packages, not system binaries.

The separation is:

| Tool | Responsibility |
|------|---------------|
| PIE | Downloads and installs the correct pre-compiled `.so` binary |
| Composer | Installs the Laravel queue driver (PHP source) and verifies `ext-rabbit_rs` is loaded |

The Laravel driver's `composer.json` declares `"ext-rabbit_rs": "^0.1"`, which causes Composer to check that the extension is loaded at install time. If the extension is missing, Composer reports the error. But Composer never installs the binary — that is PIE's role.

## Multiple PHP versions

If you have multiple PHP installations, PIE and `cargo-php` target the PHP found in your `PATH`. To target a specific PHP, run them with that PHP's interpreter and ensure its `php-config`/`phpize` come first in the `PATH`:

```bash
# With PIE (uses the php-config/phpize in PATH)
/path/to/php/bin/php /usr/local/bin/pie install goopil/rabbit-rs-native

# With cargo-php (php-config of the target PHP first in PATH)
PATH="/path/to/php/bin:$PATH" ./scripts/install.sh --release
```

## Upgrading and rollback

PIE installs are versioned replacements: installing a different version swaps the `rabbit_rs.so` binary and updates the active PHP configuration in place. Upgrades and rollbacks use the same `pie install` command with an explicit release tag.

Upgrade (or reinstall) an exact version:

```bash
pie install goopil/rabbit-rs-native:v0.1.1
```

Rollback = install the previous tag:

```bash
pie install goopil/rabbit-rs-native:v0.1.0
```

Check which version is active before and after:

```bash
php --ri rabbit_rs
```

Keep the Laravel queue driver in sync: `goopil/rabbit-rs-laravel` requires a specific `ext-rabbit_rs` major version. When moving across a major boundary — in either direction — upgrade or roll back the extension and the driver together. Composer fails loudly at `composer update` if the loaded extension does not satisfy the driver's constraint, so a half-upgraded system (new driver with old extension, or the reverse) cannot go unnoticed.

Every release exercises these paths in CI before it is finalized: the release pipeline installs the previous published release, upgrades it to the new release, and rolls back again (see [End-to-end PIE validation](#end-to-end-pie-validation)).

## Distribution model

Rabbit RS distributes two packages in synchronized releases:

- **`goopil/rabbit-rs-native`** — the native PHP extension, installed via [PIE](https://github.com/php/pie)
- **`goopil/rabbit-rs-laravel`** — the Laravel queue driver, installed via [Composer](https://getcomposer.org)

Both packages share the same version number: a release `1.2.0` produces `goopil/rabbit-rs-native 1.2.0` and `goopil/rabbit-rs-laravel 1.2.0`. The Laravel package requires `ext-rabbit_rs ^0.1` — the constraint tracks the extension version until 1.0 (see [Why Composer doesn't modify system PHP](#why-composer-doesnt-modify-system-php)).

### PIE build matrix

The CI produces **8 pre-compiled release artifacts** (V1 is NTS-only, see [Thread safety](#thread-safety)) covering all supported combinations:

| PHP | Architecture | libc | Thread safety |
|-----|-------------|------|---------------|
| 8.4 | x86_64 | glibc | NTS |
| 8.4 | x86_64 | musl | NTS |
| 8.4 | arm64 | glibc | NTS |
| 8.4 | arm64 | musl | NTS |
| 8.5 | x86_64 | glibc | NTS |
| 8.5 | x86_64 | musl | NTS |
| 8.5 | arm64 | glibc | NTS |
| 8.5 | arm64 | musl | NTS |

The matrix is defined in [`release/pie-matrix.json`](../release/pie-matrix.json).

### How pre-packaged binaries work

Each artifact is a ZIP archive containing a single `rabbit_rs.so` compiled for the exact combination of PHP version, architecture, libc, and thread-safety mode. The naming convention follows PIE's expected format, which includes the `v` prefix from the git tag:

```
php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip
```

For example:

```
php_rabbit_rs-v1.2.0_php8.5-x86_64-linux-glibc-nts.zip
```

Every Linux artifact carries an **explicit** thread-safety suffix (`-nts` in V1). PIE (1.4.10+, the version the release pipeline validates) resolves NTS assets matched either with or without the `-nts` suffix (and requires `-zts` for ZTS builds, planned for V2); the explicit suffix is the repository convention so that asset names are unambiguous and self-describing. The convention is enforced in two places that must stay consistent:

- `release/pie-matrix.json` — machine-readable matrix (`ts_suffix` is always `-nts` in V1; ZTS entries are excluded)
- `.github/workflows/release.yml` — release build (`-${{ matrix.ts }}` appended to every asset name) via the `.github/actions/package-release-asset` composite action

`scripts/validate-distribution.sh` fails if any of them drifts from the pattern expected by PIE.

macOS artifacts (`arm64-darwin-nts`) are outside the PIE matrix — `composer.json` declares `os-families: ["linux"]` — and are consumed by the Homebrew formula. macOS installs are therefore validated best-effort: the release pipeline compiles and smoke-loads both `arm64-darwin-nts` builds, and the Homebrew formula is audited and install-tested on a macOS ARM64 runner by [`homebrew-formula-test.yml`](../.github/workflows/homebrew-formula-test.yml) (formula-affecting pull requests and manual dispatch). The release pipeline itself does not reinstall through Homebrew on macOS — if that install path regresses, the formula test workflow fails on its next run rather than blocking the release.

### End-to-end PIE validation

Once the release is published, the release pipeline blocks on two verification stages against the published release:

- **`verify-pie-install`** runs a real `pie install` on every supported platform/libc combination: Linux glibc x86_64 (PHP 8.4 and 8.5) on native runners, and Linux glibc arm64, musl x86_64, and musl arm64 (PHP 8.4) inside the same digest-pinned, multi-arch PHP images the build job uses. Each job asserts that `rabbit_rs` loads and that `phpversion('rabbit_rs')` equals the release version.
- **`verify-pie-upgrade-rollback`** installs the previous published release, upgrades it to the new release, then rolls back to the previous one, asserting the installed extension version at each step. A missing previous release fails the job loudly instead of silently skipping the gate.

The Homebrew formula update and the Laravel package split run only after both stages succeed, so a release that PIE cannot install, upgrade, or roll back never reaches those channels.

Each release archive is accompanied by:

- A **SHA-256** checksum file (`.sha256`)
- A **CycloneDX SBOM** in JSON format (`.sbom.json`), generated from the `rabbit-rs-php` crate via `cargo-cyclonedx` 0.5.9
- A **GitHub build provenance attestation** (SLSA v1), signed with Sigstore using the workflow's OIDC identity
- A **GitHub SBOM attestation** binding the SBOM to the ZIP artifact

Attestations are stored in the GitHub attestations API and verified with:

    gh attestation verify <asset.zip> --repo Goopil/php-rabbit-rs \
        --predicate-type https://slsa.dev/provenance/v1

Each release therefore contains **30 assets**: 10 ZIPs, 10 SHA256 files, and 10 SBOM files, plus 20 attestations (provenance + SBOM) stored in the attestations API (not listed as release assets).

### Build characteristics

- **Static linking** — Rust dependencies and TLS libraries are linked statically into the `.so` whenever possible; the only expected system dependency is **libc** (glibc or musl). No Rust, OpenSSL, or other runtime library is needed on production systems.
- **Minimum glibc 2.31** — covers Debian 11 and later, Ubuntu 20.04 and later, CentOS 9 Stream and later. Alpine is not affected (uses musl). On an older glibc, use the musl build or compile from source.
- **No debug builds** — every release artifact is a release-optimized build, ensuring consistent performance and avoiding debug-assertion overhead.

### Release synchronization

Releases follow a strict order to ensure version coherence:

1. **CI builds all 8 PIE artifacts** — each tested with the target PHP, checksum verified
2. **Laravel package is split** — the monorepo's `packages/laravel-queue/` is split into the `Goopil/rabbit-rs-laravel` mirror repository via `scripts/split-laravel-package.sh`
3. **Native extension is tagged on Packagist** — `goopil/rabbit-rs-native` appears as a PIE package
4. **Laravel package is tagged on Packagist** — `goopil/rabbit-rs-laravel` appears as a Composer package
5. **GitHub release is published** — only after all binaries are produced, the Laravel tag is pushed, and both Packagist metadata entries are verified

The validation script [`scripts/validate-distribution.sh`](../scripts/validate-distribution.sh) checks: root package name and type (`goopil/rabbit-rs-native`, `php-ext`), extension name, download method, NTS support with the V1 ZTS exclusion, Linux-only OS family, Laravel package name and namespace, version coherence between Cargo and both packages, exactly 8 PIE matrix entries (NTS only) with unique suffixes, the minimum glibc version, the PIE asset naming convention across the packaging script, the release workflow and this document, and — when archives are present — exactly 30 files (10 ZIP + 10 SHA-256 + 10 SBOM for the 8 Linux matrix entries plus 2 macOS darwin assets) with verified checksums and CycloneDX SBOMs.

### Not V1 distribution channels

| Channel | Status | Alternative |
|---------|--------|-------------|
| PECL | Not supported | Use `pie install goopil/rabbit-rs-native` |
| Debian packages (apt) | Not maintained | Use PIE in your Dockerfile |
| RPM packages (dnf/yum) | Not maintained | Use PIE in your Dockerfile |
| APK packages (Alpine) | Not maintained | Use PIE (musl binary works on Alpine) |
| Composer plugins installing binaries | Not used | PIE handles binaries, Composer handles PHP source |
| Full PHP images bundling the extension | Not provided | Install PIE in your own Dockerfile |

This is a deliberate design decision: PIE is the PHP ecosystem's official extension installer and handles the binary dimension correctly (version matching, architecture selection, activation). Keeping binary distribution in PIE and source distribution in Composer maintains a clean separation of concerns.

## Next steps

- [Configuration reference](configuration.md)
- [Laravel usage](laravel.md)
- [Topology management](topology.md)
- [Reliability](reliability.md)
