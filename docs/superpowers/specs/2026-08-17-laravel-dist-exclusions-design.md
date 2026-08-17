# Laravel Package — Dist Archive Exclusions

**Date:** 2026-08-17
**Status:** Approved
**Scope:** `packages/laravel-queue/` only

## Problem

The Laravel package `goopil/rabbit-rs-laravel` is published to a mirror repository
(`Goopil/rabbit-rs-laravel`) via the subtree-split workflow
(`.github/workflows/split-laravel.yml` → `scripts/split-laravel-package.sh`).

The split script already excludes `composer.lock`, `vendor/`, `.git/`,
`.phpunit.cache/`, and `.gitkeep` from the mirror. It does **not** exclude
`tests/`, `phpunit.xml`, or any `.gitattributes`/`.gitignore`.

The package directory `packages/laravel-queue/` has no local `.gitattributes`.
As a result, when Composer builds the dist archive from a tag on the mirror
repository, the zip contains development-only artifacts: the full `tests/`
tree (Feature, Unit, Integration, Fixture, bootstrap, TestCase),
`phpunit.xml`, and `composer.lock`. End consumers downloading the package via
`composer require` receive test files and dev configuration they do not need.

This is confirmed by inspection: 47 tracked files in `packages/laravel-queue/`,
no local `.gitattributes`, no local `.gitignore`. The root `.gitattributes`
already excludes `packages/` from the native extension dist, so it does not
apply to the split Laravel package.

## Goal

Ensure the Composer dist archive of `goopil/rabbit-rs-laravel` contains only
runtime-relevant files: `src/`, `config/`, `composer.json`, `README.md`, and the
MIT `LICENSE` (when present). Exclude development-only artifacts from the dist
archive without altering the split script's behavior or the root package.

## Non-Goals

- Modifying `scripts/split-laravel-package.sh`. The script's `find` exclusions
  are left untouched; `tests/` and `phpunit.xml` remain in the mirror's git
  history (visible on `git clone`) but absent from the dist zip.
- Adding a local `.gitignore` to `packages/laravel-queue/`. `.gitignore` has no
  effect on Composer dist archives (which are built from git-tracked files, not
  from the working tree), and the root `.gitignore` already covers
  `.phpunit.cache/` and `vendor/` patterns.
- Touching the root `.gitattributes` or the native extension PIE dist. The root
  `.gitattributes` already excludes `packages/` via `export-ignore`, so the
  Laravel subtree is already absent from the native ext archive.

## Design

### Mechanism: local `.gitattributes` with `export-ignore`

Git attributes are the standard Composer mechanism for shaping dist archives:
when Composer creates a zip from a git tag, it invokes `git archive`, which
honors `export-ignore` attributes. A local `.gitattributes` placed in
`packages/laravel-queue/` is copied by the split script to the root of the
mirror repository, where it takes effect for every tag produced there.

### File to create

`packages/laravel-queue/.gitattributes`:

```
# Exclude development-only files from Composer dist archives.
/tests export-ignore
/phpunit.xml export-ignore
/composer.lock export-ignore
/.gitattributes export-ignore
```

### Rationale per entry

- `/tests` — all test suites (Feature, Unit, Integration, Fixture), `TestCase.php`,
  `bootstrap.php`. Not needed at runtime.
- `/phpunit.xml` — PHPUnit configuration. Not needed at runtime.
- `/composer.lock` — a library should not ship its lock file; consumers
  resolve their own dependencies. The split script already excludes it from
  the mirror, but the entry is belt-and-suspenders in case the script changes.
- `/.gitattributes` — self-exclusion is idiomatic; the dist archive should not
  contain packaging metadata.

### Paths not excluded

- `src/`, `config/`, `composer.json`, `README.md`, `LICENSE` — runtime files,
  must ship.
- `vendor/` — already git-ignored and excluded by the split script; no entry
  needed.
- `.phpunit.cache/` — already git-ignored and excluded by the split script; no
  entry needed.

## Verification

1. `git check-attr -a packages/laravel-queue/tests/Unit/RabbitMqJobTest.php`
   must report `export-ignore: set` (the `/tests` pattern matches sub-paths).
2. `git check-attr -a packages/laravel-queue/phpunit.xml` must report
   `export-ignore: set`.
3. `git check-attr -a packages/laravel-queue/composer.lock` must report
   `export-ignore: set`.
4. `rtk composer validate --strict` in `packages/laravel-queue` must remain
   PASS (`.gitattributes` does not affect `composer validate`, which reads only
   `composer.json`).
5. `./scripts/split-laravel-package.sh --dry-run` must still list
   `.gitattributes` among the files that would be copied to the mirror root
   (the `find -type f` invocation picks it up; no exclusion rule matches it).

## Impact

- **Dist archive:** `tests/`, `phpunit.xml`, `composer.lock`, `.gitattributes`
  are absent from the zip Composer downloads for `goopil/rabbit-rs-laravel`.
- **Mirror git history:** unchanged. `tests/` and `phpunit.xml` remain visible
  on `git clone` of `Goopil/rabbit-rs-laravel`. This is acceptable: the split
  script is intentionally non-destructive, and the contract is about dist
  archives, not mirror history.
- **Native extension PIE dist:** unaffected. The root `.gitattributes` already
  excludes `packages/`.
- **CI:** unaffected. `.gitattributes` is a static metadata file; no workflow
  reads it at build time except `git archive` during dist generation.
