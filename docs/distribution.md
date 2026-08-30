# Distribution

Rabbit RS distributes two packages in synchronized releases:

- **`goopil/rabbit-rs-native`** — the native PHP extension, installed via [PIE](https://github.com/php/pie)
- **`goopil/rabbit-rs-laravel`** — the Laravel bridge, installed via [Composer](https://getcomposer.org)

Both packages share the same version number. A release `1.2.0` produces `goopil/rabbit-rs-native 1.2.0` and `goopil/rabbit-rs-laravel 1.2.0`. The Laravel package requires `ext-rabbit_rs ^0.0` (the constraint tracks the extension version until 1.0).

## PIE build matrix

The CI produces **8 pre-compiled release artifacts** (V1 is NTS-only, see [Thread safety: NTS only in V1](#thread-safety-nts-only-in-v1)) covering all supported combinations:

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

## Thread safety: NTS only in V1

V1 ships **NTS binaries only**. ZTS (thread-safe PHP) is deliberately excluded from the release matrix, and the root `composer.json` declares `"support-zts": false`.

Rationale: the extension keeps a process-global runtime and connection registry. Under ZTS that registry is shared across PHP threads, and TSRM (Zend Thread Safety) per-thread isolation is not implemented in V1 — shipping ZTS binaries would expose shared state to unsynchronized concurrent access. The previous advisory ZTS CI job (`continue-on-error`) only proved that a ZTS binary loads, not that it is safe under real concurrency.

ZTS is planned for **V2**, gated on three prerequisites: per-thread isolation (a TSRM-aware registry), a blocking ZTS CI job (no `continue-on-error`), and real concurrency tests. The decision is tracked in [`docs/plans/ROADMAP.md`](plans/ROADMAP.md) (Parked — ZTS).

## How pre-packaged binaries work

Each artifact is a ZIP archive containing a single `rabbit_rs.so` compiled for the exact combination of PHP version, architecture, libc, and thread-safety mode. The naming convention follows PIE's expected format, which includes the `v` prefix from the git tag:

```
php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip
```

For example:

```
php_rabbit_rs-v1.2.0_php8.5-x86_64-linux-glibc-nts.zip
```

### Unified thread-safety suffix

Every Linux artifact carries an **explicit** thread-safety suffix (`-nts` in V1). PIE (1.5.x) resolves NTS assets matched either with or without the `-nts` suffix (and requires `-zts` for ZTS builds, planned for V2); the explicit suffix is the repository convention so that asset names are unambiguous and self-describing. The convention is enforced in three places that must stay consistent:

- `scripts/package-pie-binary.sh` — packaging script (self-tested via `--self-test`)
- `release/pie-matrix.json` — machine-readable matrix (`ts_suffix` is always `-nts` in V1; ZTS entries are excluded)
- `.github/workflows/release.yml` — release build (`-${{ matrix.ts }}` appended to every asset name)

`scripts/verify-pie-naming.sh` fails if any of them drifts from the pattern expected by PIE.

macOS artifacts (`arm64-darwin-nts`) are outside the PIE matrix — `composer.json` declares `os-families: ["linux"]` — and are consumed by the Homebrew formula.

### End-to-end PIE validation

Before a release is published, the release pipeline runs a blocking `verify-pie-install` job that installs the drafted release with a real `pie install` (PHP 8.4, x86_64, glibc, NTS) by resolving the package through the GitHub API against the draft release. Publication is blocked unless PIE successfully matches, downloads, installs, and loads the extension with the released version.

Each release archive is accompanied by:

- A **SHA-256** checksum file (`.sha256`)
- A **CycloneDX SBOM** in JSON format (`.sbom.json`), generated from the
  `rabbit-rs-php` crate via `cargo-cyclonedx` 0.5.9
- A **GitHub build provenance attestation** (SLSA v1), signed with Sigstore
  using the workflow's OIDC identity
- A **GitHub SBOM attestation** binding the SBOM to the ZIP artifact

Attestations are stored in the GitHub attestations API and verified with:

    gh attestation verify <asset.zip> --repo Goopil/rabbit-rs \
        --predicate-type https://slsa.dev/provenance/v1

Each release therefore contains **30 assets**: 10 ZIPs, 10 SHA256 files, and
10 SBOM files, plus 20 attestations (provenance + SBOM) stored in the
attestations API (not listed as release assets).

PIE inspects your PHP installation, determines the correct artifact, downloads it, verifies the checksum, copies the `.so` to your extension directory, and enables it in your PHP configuration.

### Static linking

Rust dependencies and TLS libraries are linked statically into the `.so` whenever possible. The only expected system dependency is **libc** (glibc or musl). This means you do not need to install Rust, OpenSSL, or any other runtime library on your production system.

## Minimum glibc

The glibc builds target **glibc 2.31** as the minimum baseline. This covers:

- Debian 11 (Bullseye) and later
- Ubuntu 20.04 and later
- Alpine is not affected (uses musl, not glibc)
- CentOS 9 Stream and later

If your distribution ships an older glibc, use the musl build or compile from source.

## No debug builds

Debug builds are not distributed. Every release artifact is a release-optimized build. This ensures consistent performance and avoids the overhead of debug assertions.

## Release synchronization

Releases follow a strict order to ensure version coherence:

1. **CI builds all 8 PIE artifacts** — each tested with the target PHP, checksum verified
2. **Laravel package is split** — the monorepo's `packages/laravel-queue/` is split into the `Goopil/rabbit-rs-laravel` mirror repository via `scripts/split-laravel-package.sh`
3. **Native extension is tagged on Packagist** — `goopil/rabbit-rs-native` appears as a PIE package
4. **Laravel package is tagged on Packagist** — `goopil/rabbit-rs-laravel` appears as a Composer package
5. **GitHub release is published** — only after all binaries are produced, the Laravel tag is pushed, and both Packagist metadata entries are verified

The validation script [`scripts/validate-distribution.sh`](../scripts/validate-distribution.sh) checks:
- Root package name and type (`goopil/rabbit-rs-native`, `php-ext`)
- Extension name (`rabbit_rs`)
- Download method (`pre-packaged-binary`)
- NTS support and the V1 ZTS exclusion (`support-zts: false`)
- Linux-only OS family
- Laravel package name and namespace
- Major version coherence between Cargo, the extension, and the Laravel package
- Exactly 8 PIE matrix entries (NTS only) with unique suffixes
- Minimum glibc version

## Not V1 distribution channels

The following are **not** distribution channels for V1:

| Channel | Status | Alternative |
|---------|--------|-------------|
| PECL | Not supported | Use `pie install goopil/rabbit-rs-native` |
| Debian packages (apt) | Not maintained | Use PIE in your Dockerfile |
| RPM packages (dnf/yum) | Not maintained | Use PIE in your Dockerfile |
| APK packages (Alpine) | Not maintained | Use PIE (musl binary works on Alpine) |
| Composer plugins installing binaries | Not used | PIE handles binaries, Composer handles PHP source |
| Full PHP images bundling the extension | Not provided | Install PIE in your own Dockerfile |

This is a deliberate design decision. PIE is the PHP ecosystem's official extension installer and handles the binary dimension correctly (version matching, architecture selection, activation). Keeping binary distribution in PIE and source distribution in Composer maintains a clean separation of concerns.

## Package metadata

### Root `composer.json` (PIE package)

```json
{
    "name": "goopil/rabbit-rs-native",
    "type": "php-ext",
    "require": {
        "php": "^8.4"
    },
    "php-ext": {
        "extension-name": "rabbit_rs",
        "priority": 80,
        "support-zts": false,
        "support-nts": true,
        "os-families": ["linux"],
        "download-url-method": ["pre-packaged-binary"]
    }
}
```

### Laravel `composer.json` (Composer package)

```json
{
    "name": "goopil/rabbit-rs-laravel",
    "type": "library",
    "require": {
        "php": "^8.4",
        "ext-rabbit_rs": "^0.0",
        "illuminate/queue": "^12.0 || ^13.0"
    }
}
```

The `ext-rabbit_rs` constraint tracks the extension version until 1.0. Composer checks that the extension is loaded but never installs it.

## GitHub repositories

| Repository | Purpose |
|-----------|---------|
| [Goopil/rabbit-rs](https://github.com/Goopil/rabbit-rs) | Monorepo (source of truth) |
| [Goopil/rabbit-rs-laravel](https://github.com/Goopil/rabbit-rs-laravel) | Laravel package mirror (read-only, auto-split) |

The monorepo is the development source. A CI workflow splits `packages/laravel-queue/` into the mirror repository on every release tag. The mirror exists so Packagist can consume the Laravel package as a standalone repository.
