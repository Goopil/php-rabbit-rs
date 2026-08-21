# Code Coverage Plan

**Goal:** Add code coverage measurement and reporting for Rust core, PHP extension, and Laravel package.

## Context

After the test refactor (Pest migration, Criterion decommission), coverage measurement is the natural next step to identify untested code paths and prevent regressions.

## Scope

| Part | Tool | CI Time | Complexity |
|------|------|---------|------------|
| Rust core | `cargo-tarpaulin` or `cargo-llvm-cov` | ~5 min | Trivial |
| PHP extension (Pest) | `pest --coverage` with Xdebug/PCOV | ~2 min | Simple |
| Laravel package (Pest) | `pest-plugin-coverage` with PCOV | ~1 min | Simple |
| CI integration + badges | Codecov or GitHub Pages | ~5 min | Medium |

## Phase 1: Rust Coverage

### Task 1: Add cargo-tarpaulin to CI

- Add a `coverage` job to `.github/workflows/ci.yml`
- Install `cargo-tarpaulin` via `cargo install cargo-tarpaulin`
- Run: `cargo tarpaulin --workspace --all-features --out xml --output-dir coverage/`
- Upload `coverage/cobertura.xml` to Codecov via `codecov/codecov-action@v4`
- Consider `cargo-llvm-cov` as alternative (faster, built into rust toolchain)

### Task 2: Add coverage badge to README

- Add Codecov badge to README.md

## Phase 2: PHP Extension Coverage

### Task 3: Add coverage to extension Pest tests

- Install `pcov` extension in CI (faster than Xdebug for coverage)
- Or use `xdebug.mode=coverage` if PCOV unavailable
- Run: `php -d extension=$ARTIFACT -d pcov.enabled=1 vendor/bin/pest --coverage --min=80`
- Upload coverage report

### Task 4: Add coverage to extension CI job

- Modify `phpt` job or add separate `coverage-php` job
- Upload Pest coverage report (Clover format) to Codecov

## Phase 3: Laravel Package Coverage

### Task 5: Add pest-plugin-coverage

- Add `pestphp/pest-plugin-coverage` to `packages/laravel-queue/composer.json` require-dev
- Run: `php vendor/bin/pest --coverage --min=80`
- The plugin auto-detects PCOV or Xdebug

### Task 6: Add coverage to Laravel CI job

- Setup PHP with `coverage: pcov` in `shivammathur/setup-php`
- Run `./scripts/test-laravel.sh --coverage` (add flag to script)
- Upload coverage report to Codecov

## Phase 4: Consolidation

### Task 7: Single Codecov upload

- Merge Rust + PHP coverage reports into a single Codecov upload
- Or upload separately with flags (`rust`, `php-extension`, `laravel`)

### Task 8: Coverage gates

- Add `--min` coverage thresholds to prevent regressions
- Start at current coverage level, increase incrementally
- Document coverage requirements in AGENTS.md

## Dependencies

- Codecov account (free for OSS) or alternative: Coveralls, SonarQube
- `cargo-tarpaulin` or `cargo-llvm-cov` (Rust)
- `pcov` PHP extension (faster than Xdebug for coverage-only)
- `pestphp/pest-plugin-coverage` (Laravel package)

## Estimated Time

- Phase 1 (Rust): 30 min
- Phase 2 (PHP extension): 30 min
- Phase 3 (Laravel): 30 min
- Phase 4 (Consolidation): 30 min
- Total: ~2 hours

## Open Questions

- Codecov vs Coveralls vs SonarQube?
- `cargo-tarpaulin` (standalone, slower) vs `cargo-llvm-cov` (needs `llvm-tools-preview` component, faster)?
- Coverage threshold starting point? (need to measure current coverage first)
- Should coverage block PRs, or be informational only initially?
