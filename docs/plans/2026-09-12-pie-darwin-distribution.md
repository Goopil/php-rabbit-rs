# PIE macOS (darwin) distribution — design

Date: 2026-09-12
Status: Approved (see conversation summary at the end)

## Context

The V1 design (`2026-07-30-rabbitmq-native-design.md`) excluded macOS as a
distributed production platform. macOS binaries were still built by the release
pipeline (`build-macos`, 2 artifacts per release) but kept **outside** the PIE
matrix: `composer.json` declared `os-families: ["linux"]`, so `pie install`
refused to run on macOS ("This extension does not support the darwin operating
system family"). macOS users were routed to Homebrew or a manual download,
validated best-effort.

PIE supports macOS fully (verified against `php/pie` 1.4.10 and 1.5.x source),
and a peer project (`BSN4/grpc-php-rs`) ships the same convention in
production.

## PIE mechanics on macOS

- `OperatingSystemFamily::Darwin = "darwin"` — accepted by PIE's
  `php-ext.os-families` metadata.
- Asset naming (`PrePackagedBinaryAssetName::packageNames`) interpolates
  `{os}-{libc}{debug}{ts}`. On macOS, `LibcFlavour::detect()` returns `bsdlibc`
  when `otool` is on PATH (true on every machine with Xcode Command Line
  Tools, i.e. every Homebrew/dev machine), and `glibc` otherwise. The release
  assets therefore carry the explicit `bsdlibc` token.
- Identical behavior in PIE 1.4.10 (the pinned CI version) and 1.5.x.

## Decision

Ship PIE support on macOS with a **single name per binary**:

- `composer.json`: `os-families: ["linux", "darwin"]`.
- macOS asset name: `php_rabbit_rs-v{version}_php{php}-arm64-darwin-bsdlibc-nts.zip`
  (renamed from `...-arm64-darwin-nts.zip`); the Homebrew formula consumes the
  same renamed assets, so there is one URL per binary.
- `release/pie-matrix.json` gains an `os` field on every entry plus 2 darwin
  entries (PHP 8.4/8.5, arm64, bsdlibc, nts). Matrix: 10 entries.
- `verify-pie-install` gains a `macOS arm64 PHP 8.4` cell (`macos-14` runner)
  so a release that PIE cannot install on macOS blocks the pipeline. The PIE
  phar checksum step becomes portable (`sha256sum` on Linux, `shasum -a 256`
  on macOS).
- `scripts/validate-distribution.sh` enforces: 2 os-families (linux, darwin),
  10 matrix entries, PIE candidate names parameterized by `{os}` (the
  `anylibc` fallback stays Linux-only), and the renamed workflow naming.
- Docs: `reference.md` documents the generalized
  `php{php}-{arch}-{os}-{libc}-{ts}` naming and the macOS PIE install path;
  `README.md` drops the "PIE does not support macOS" statement.

## Alternatives rejected

- Publish a duplicate `...-darwin-glibc-nts.zip` asset for macOS machines
  without Xcode CLT (no `otool`): rejected — doubles the macOS assets for a
  corner case; such machines cannot have Homebrew PHP either, and the PIE
  failure ("could not find a matching asset") is clear.
- Keep the old asset name for Homebrew and add a bsdlibc-named duplicate:
  rejected — two URLs for one binary guarantees drift and inflates the
  inventory to 12 ZIPs.

## Impact

Inventory per release is unchanged: 10 ZIPs / 30 files. The only consumer of
the old macOS asset URLs is the Homebrew formula, regenerated at each release
by `scripts/update-homebrew-formula.sh` (updated in the same change).

## Conversation summary

- Gap analysis: macOS artifacts existed but were excluded from PIE by
  `os-families: ["linux"]` and a missing libc token in the asset name; the
  release gate (`verify-pie-install`) had no macOS cell.
- User choices: bsdlibc-only coverage (no glibc duplicate asset); approach A
  (rename everywhere + macOS PIE gate cell).
- External validation: `BSN4/grpc-php-rs` publishes
  `php_grpc-v0.4.0_php8.4-arm64-darwin-bsdlibc.zip` and declares its OS
  families via `os-families-exclude`. We keep our explicit
  `os-families: ["linux", "darwin"]` (matches `validate-distribution.sh`)
  and our explicit `-nts` suffix (repo convention).
