# SonarCloud Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all 204 SonarCloud issues (10 bugs, 46 vulnerabilities, 148 code smells) on the **`Goopil_php-rabbit-rs`** project (revised 2026-08-29 decision — the repo was renamed `Goopil/php-rabbit-rs`, see Re-validation) without degrading hot-path performance.

**Architecture:** Fixes are grouped by rule category and file cluster, ordered from lowest-risk/trivial to highest-risk/structural. Each task is self-contained and independently testable. Performance-critical code (RabbitMqQueue, benchmarks) uses annotation-based suppression rather than refactoring.

**Tech Stack:** PHP 8.4/8.5, Rust 1.96, GitHub Actions, Docker, Bash/Shell

## Re-validation 2026-08-29

Current distribution verified via API (project `Goopil_php-rabbit-rs`, autoscan):
**204 issues** — 150 PHP, 37 GitHubActions, 8 Docker, 8 Shell, 1 JSON. **Rust not
analyzed** (Sonar Rust plugin = Enterprise; 0 Rust issues expected). The per-rule
distribution is nearly identical to the initial plan (203 → 204, ±1): the 19 tasks remain
valid.

Observed SonarCloud situation (two parallel projects):

| | `Goopil_php-rabbit-rs` | `Goopil_rabbit-rs` |
|---|---|---|
| Trigger | autoscan GitHub App | CI job `coverage.yml` |
| Issues | 204 (real issues) | 0 |
| Coverage | ❌ none | ✅ rust.lcov + php-ext.lcov + clover |
| PR decoration | ✅ | ❌ NOT_BOUND |
| Quality Gate | ERROR | — |

**Settled decision (revised 2026-08-29): single project = `Goopil_php-rabbit-rs` (autoscan).**
Initially `Goopil_rabbit-rs` (CI), the decision was reversed the same day: the GitHub repo
was renamed `Goopil/php-rabbit-rs`, which re-aligns the autoscan project key
with the repo name. Full switch to autoscan:
- CI analysis is **removed** (autoscan and CI analysis are mutually exclusive on
  the same Sonar project — the job would be rejected with "CI analysis while Automatic Analysis is
  enabled") → Task 14b.
- Consequence accepted by the user: SonarCloud no longer receives coverage
  (autoscan does not consume reports). Codecov is unaffected (direct uploads
  in the coverage jobs).
- The table above is kept as a historical snapshot of the morning of 08/29.
- `SONAR_TOKEN` becomes unused by CI (removal on the GitHub side is at the user's
  discretion, out of scope).

## Global Constraints

- Rust is pinned to 1.96.0, edition 2024
- Unsafe Rust is forbidden — never weaken `#![forbid(unsafe_code)]`
- PHP tests use Pest (not PHPUnit)
- Run `rtk cargo fmt --all` after Rust edits
- Run focused tests then full gate: `rtk ./scripts/check.sh`
- Do not commit `.air/`, IDE metadata, build artifacts
- Preserve unrelated work in a dirty tree
- Performance-sensitive code: use `@phpstan-ignore` / `@noinspection` annotations instead of structural refactoring
- Stubs file is manually maintained — use PHPDoc comments, not real implementations
- Lab credentials are local-only and safe — document, don't replace

---

## Task 0: SonarCloud consolidation — single project `Goopil_rabbit-rs`

> **⚠️ AMENDED 2026-08-29 (revised decision, see Re-validation):** the target becomes
> the autoscan `Goopil_php-rabbit-rs`. Step 1 below is **void** (the `sonarcloud`
> job is removed by Task 14b — the action replacement done in 345a2dd
> remains correct for the record). The UI actions become: **(1)** bind nothing
> (autoscan is already linked via the GitHub App, automatic PR decoration); **(2)**
> delete the `Goopil_rabbit-rs` project (now orphaned) — recommended after the MR
> merge, not before. Step 3 becomes: verify the autoscan PR decoration on the
> MR (sonarqubecloud comment present).

**Original decision (2026-08-29, morning):** reference project = `Goopil_rabbit-rs`. The
autoscan project `Goopil_php-rabbit-rs` (repo name before the rename, 204 issues) is
abandoned.

**Files:**
- Modify: `.github/workflows/coverage.yml` (deprecated action to replace)
- Verify: `sonar-project.properties` (key already correct — no change expected)

**SonarCloud UI actions (manual, user):**
1. `Goopil_rabbit-rs` > Administration > DevOps Platform Integration > GitHub:
   bind the project to the `Goopil/rabbit-rs` repo → activates PR decoration + PR Quality Gate.
2. Delete the `Goopil_php-rabbit-rs` project. If autoscan recreates it on the next
   push, disable AutoScan on that project (project Administration).

**Interfaces:**
- Consumes: `SONAR_TOKEN` secret (existing), artifacts of the 3 coverage jobs
- Produces: CI scan on the right project, active PR decoration, working Quality Gate

- [ ] **Step 1: Replace the deprecated action**

In `.github/workflows/coverage.yml`, job `sonarcloud`:
`SonarSource/sonarcloud-github-action@master` → `SonarSource/sonarqube-scan-action@<version>`
(the current action emits a deprecation warning; the full SHA pin is done in
Task 2).

- [ ] **Step 2: Verify the configuration**

`sonar-project.properties`: key `Goopil_rabbit-rs` ✓, coverage paths
(`target/coverage/rust.lcov`, `target/coverage/php-ext.lcov`,
`packages/laravel-queue/build/clover.xml`) aligned with the artifacts downloaded by
the job ✓. Change nothing if compliant.

- [ ] **Step 3: Post-binding verification (after user UI actions)**

On a PR of the branch: sonarqubecloud comment present, Quality Gate not ERROR, coverage
measures > 0 on `sonarcloud.io/dashboard?id=Goopil_rabbit-rs`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/coverage.yml
git commit -m "ci: replace deprecated sonarcloud-github-action with sonarqube-scan-action"
```

---

## Task 1: Add trailing newlines to all PHP files (S113)

**Files:**
- Modify: `packages/laravel-queue/src/Connectors/RabbitMqConnector.php`
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php`
- Modify: `packages/laravel-queue/src/Jobs/RabbitMqJob.php`
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php`
- Modify: `packages/laravel-queue/src/Support/NativePoolFactory.php`
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php`
- Modify: `packages/laravel-queue/src/Support/MessageMapper.php`
- Modify: `packages/laravel-queue/src/Support/WorkerProfileResolver.php`
- Modify: `packages/laravel-queue/src/Exceptions/QueueException.php`
- Modify: `packages/laravel-queue/tests/bootstrap.php`
- Modify: `packages/laravel-queue/tests/TestCase.php`
- Modify: `crates/rabbit-rs-php/tests/fixtures/fpm/index.php`

**Interfaces:**
- Consumes: None
- Produces: All listed files end with exactly one trailing newline

- [ ] **Step 1: Add trailing newline to each file**

For each file listed above, ensure the file ends with a single `\n` after the last non-empty line. Most files are missing the final newline entirely.

Use the Edit tool to add a newline at the end of each file.

- [ ] **Step 2: Verify all files end with newline**

Run: `for f in packages/laravel-queue/src/Connectors/RabbitMqConnector.php packages/laravel-queue/src/RabbitMqQueue.php packages/laravel-queue/src/Jobs/RabbitMqJob.php packages/laravel-queue/src/Config/ConfigNormalizer.php packages/laravel-queue/src/Support/NativePoolFactory.php packages/laravel-queue/src/RabbitMqServiceProvider.php packages/laravel-queue/src/Support/MessageMapper.php packages/laravel-queue/src/Support/WorkerProfileResolver.php packages/laravel-queue/src/Exceptions/QueueException.php packages/laravel-queue/tests/bootstrap.php packages/laravel-queue/tests/TestCase.php crates/rabbit-rs-php/tests/fixtures/fpm/index.php; do tail -c1 "$f" | od -An -tx1 | grep -q 0a || echo "MISSING NEWLINE: $f"; done`

Expected: No output (all files pass)

- [ ] **Step 3: Run Pest tests to verify no breakage**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --testdox`

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add packages/laravel-queue/src/Connectors/RabbitMqConnector.php packages/laravel-queue/src/RabbitMqQueue.php packages/laravel-queue/src/Jobs/RabbitMqJob.php packages/laravel-queue/src/Config/ConfigNormalizer.php packages/laravel-queue/src/Support/NativePoolFactory.php packages/laravel-queue/src/RabbitMqServiceProvider.php packages/laravel-queue/src/Support/MessageMapper.php packages/laravel-queue/src/Support/WorkerProfileResolver.php packages/laravel-queue/src/Exceptions/QueueException.php packages/laravel-queue/tests/bootstrap.php packages/laravel-queue/tests/TestCase.php crates/rabbit-rs-php/tests/fixtures/fpm/index.php
git commit -m "fix: add trailing newlines to PHP files (SonarCloud S113)"
```

---

## Task 2: Fix GitHub Actions — pin all action versions to full commit SHA (S7637)

**Files:**
- Modify: `.github/workflows/ci.yml` (lines 22, 25, 52, 55, 61, 84, 87, 92, 109, 112, 153, 246, 274, 280, 286)
- Modify: `.github/workflows/coverage.yml` (lines 22, 25, 31, 45, 53, 77, 83, 86, 135, 156, 177, 185, 244)
- Modify: `.github/workflows/release.yml` (lines 28, 70, 199, 202, 209, 286, 345, 397, 428)
- Modify: `.github/workflows/homebrew-formula-test.yml`

**Interfaces:**
- Consumes: None
- Produces: All `uses:` directives reference full commit SHA hashes instead of tag versions

- [ ] **Step 1: Resolve commit SHAs for each action**

For each `uses: action@version` directive, look up the full commit SHA for the pinned version. Use `gh api` or the GitHub API to resolve tags to SHAs.

Actions to resolve:
- `actions/checkout@v7` → full SHA
- `dtolnay/rust-toolchain@stable` → full SHA
- `Swatinem/rust-cache@v2` → full SHA
- `shivammathur/setup-php@v2` → full SHA
- `actions/cache@v6` → full SHA
- `codecov/codecov-action@v5` → full SHA
- `actions/upload-artifact@v4` → full SHA
- `actions/download-artifact@v4` → full SHA
- `SonarSource/sonarcloud-github-action@master` → handled in Task 0 (replaced by
  `SonarSource/sonarqube-scan-action`, then SHA-pinned)
- `github/codeql-action/upload-sarif@v3` → full SHA
- `actions/attest@v4` → full SHA

Run: `gh api repos/actions/checkout/git/refs/tags/v7 --jq '.object.sha'` (repeat for each)

- [ ] **Step 2: Replace all tag references with SHA references**

Update every `uses:` directive across all workflow files. Format: `uses: actions/checkout@<full-sha>`

For `dtolnay/rust-toolchain@stable`, resolve the `stable` branch HEAD commit.

- [ ] **Step 3: Validate YAML syntax**

Run: `for f in .github/workflows/*.yml; do python3 -c "import yaml; yaml.safe_load(open('$f'))" && echo "OK: $f"; done`

Expected: All files parse successfully

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/
git commit -m "fix: pin GitHub Actions to full commit SHAs (SonarCloud S7637)"
```

---

## Task 3: Fix GitHub Actions — lock-file enforcement (S8546, S8549)

**Files:**
- Modify: `.github/workflows/ci.yml` (lines 137, 255, 296, 301)
- Modify: `.github/workflows/coverage.yml` (lines 101, 106, 165)

**Interfaces:**
- Consumes: None
- Produces: Composer/cargo install commands use `--locked` flag

- [ ] **Step 1: Add `--locked` to cargo install commands**

In `ci.yml`:
- Line 137: `cargo install cargo-nextest --locked --quiet || true` (already has `--locked`, check for lines missing it)
- Lines 255, 301: composer update commands — add `--no-interaction` or verify lock-file presence

In `coverage.yml`:
- Line 101: `cargo build` with coverage — add `--locked` to any `cargo install` commands
- Line 106: `composer update` — this is intentionally without lock for matrix testing. Add a comment explaining this is a CI matrix test that must test against latest deps.

For `composer update` commands that are intentionally unlocked (matrix testing against multiple Laravel versions), add inline comments: `# sonar:no-lock CI matrix test — intentional` to mark as reviewed.

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/
git commit -m "fix: enforce lock-file for dependency installs (SonarCloud S8546, S8549)"
```

---

## Task 4: Fix GitHub Actions — move workflow-level write permissions to job level (S8233)

**Files:**
- Modify: `.github/workflows/release.yml` (lines 9-12)

**Interfaces:**
- Consumes: None
- Produces: Permissions moved from workflow level to each job that needs them

- [ ] **Step 1: Remove workflow-level permissions block**

Remove lines 8-12 in `release.yml`:
```yaml
permissions:
  contents: write
  id-token: write
  attestations: write
  artifact-metadata: write
```

- [ ] **Step 2: Add job-level permissions**

Add `permissions:` block to each job that needs write access:

`create-release` job: add `permissions: contents: write`
`build-linux` job: add `permissions: contents: write, attestations: write, id-token: write, artifact-metadata: write`
`build-macos` job: add `permissions: contents: write, attestations: write, id-token: write, artifact-metadata: write`
`verify-release` job: already has `permissions: contents: write, attestations: read`
`publish-release` job: add `permissions: contents: write`
`split-laravel` job: already has `permissions: contents: read`
`update-homebrew-formula` job: add `permissions: contents: read` (the tap push
uses MIRROR_TOKEN, not GITHUB_TOKEN — least privilege, per the Task 4 ruling review)
`test-homebrew-formula` job: no write needed

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "fix: move workflow-level permissions to job level (SonarCloud S8233)"
```

---

## Task 5: Fix shell scripts — positional parameters, default case, stderr (S7679, S131, S7677, S7688)

**Files:**
- Modify: `scripts/update-homebrew-formula.sh` (lines 75, 77, 102)
- Modify: `scripts/verify-release-assets.sh` (lines 69, 71)
- Modify: `scripts/test-laravel.sh` (line 53)
- Modify: `scripts/test-octane.sh` (line 14)
- Modify: `benchmarks/run-benchmarks.sh` (line 12)

**Interfaces:**
- Consumes: None
- Produces: Shell scripts with local variables for positional params, default cases in switch, stderr redirects

- [ ] **Step 1: Assign positional parameters to local variables in update-homebrew-formula.sh**

Lines 75, 77 reference `$1` directly in `sha256_func()`. Change to:
```bash
sha256_func() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    else
        shasum -a 256 "$file" | awk '{print $1}'
    fi
}
```

- [ ] **Step 2: Same for verify-release-assets.sh lines 69, 71**

The `sha256_func()` function uses `$1`. Same fix as above.

- [ ] **Step 3: Add default case (*) to test-laravel.sh switch**

Line 53 has a `case` without a default. Add:
```bash
case "${arg}" in
    tests/Integration|tests/Integration/*)
        NEED_EXTENSION=true
        ;;
    *)
        ;;
esac
```

- [ ] **Step 4: Redirect error message to stderr in test-octane.sh**

Line 14: change `echo "..."` to `echo "..." >&2`

- [ ] **Step 5: Use [[ instead of [ in benchmarks/run-benchmarks.sh**

Line 12: change `[ ... ]` to `[[ ... ]]`

- [ ] **Step 6: Fix HTTPS redirect for update-homebrew-formula.sh**

Line 102: the `curl` command already uses `-fsSL`. Add `--retry 3 --retry-delay 5` and ensure it follows HTTPS only. The URL is already HTTPS. Mark as safe with a comment if needed.

- [ ] **Step 7: Commit**

```bash
git add scripts/ benchmarks/run-benchmarks.sh
git commit -m "fix: shell script quality issues (SonarCloud S7679, S131, S7677, S7688)"
```

---

## Task 6: Fix stubs file — add PHPDoc comments for empty methods and unused params (S1186, S1172)

**Files:**
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`

**Interfaces:**
- Consumes: None
- Produces: Stub file with PHPDoc comments explaining empty method bodies and marking unused params as intentional

- [ ] **Step 1: Add comments to empty methods (S1186)**

For each empty method in the stub file, add a PHPDoc comment explaining why it's empty. These are stub methods — the real implementation is in the Rust extension.

Methods to annotate (line numbers from SonarCloud):
- Line 21: `Pool::__construct` — add `/** {@inheritDoc} Implemented by ext-rabbit_rs native code. */`
- Line 46: `Pool::consumer` — same pattern
- Line 57: `Pool::size` — same pattern
- Line 61: `Pool::clear` — same pattern
- Line 89: `Pool::close` — same pattern
- Line 96: `Consumer::next` — same pattern
- Line 151: `Consumer::close` — same pattern
- Line 158: `Delivery::payload` — same pattern

Use this comment pattern for each:
```php
/**
 * Implemented by the ext-rabbit_rs native extension.
 * @see \Goopil\RabbitRs\Pool Method is provided by the C extension at runtime.
 */
```

- [ ] **Step 2: Mark unused parameters as intentional (S1172)**

For each method with unused parameters (SonarCloud flags these because the method body is empty), add `@noinspection PhpUnusedParameterInspection` to the PHPDoc block. This is a JetBrains annotation recognized by SonarCloud.

Parameters flagged:
- Line 21: `$config` in `Pool::__construct`
- Line 31: `$message` in `Pool::publish`
- Line 42: `$messages` in `Pool::publishBatch`
- Line 46: `$profile` in `Pool::consumer`
- Line 57: `$broker, $queue` in `Pool::size`
- Line 61: `$broker, $queue` in `Pool::clear`
- Line 73: `$callback` in `Pool::onConnectionState`
- Line 85: `$callback` in `Pool::onBackpressure`
- Line 96: `$timeoutMs` in `Consumer::next`
- Line 116: `$max, $timeoutMs` in `Consumer::nextBatch`
- Line 125: `$delivery` in `Consumer::ackThrough`
- Line 136: `$deliveries` in `Consumer::ackBatch`
- Line 188: `$delayMs` in `Delivery::release`
- Line 195: `$requeue` in `Delivery::reject`

For each, add the annotation to the existing PHPDoc or create one:
```php
/**
 * Implemented by ext-rabbit_rs.
 * @noinspection PhpUnusedParameterInspection
 */
```

- [ ] **Step 3: Validate stub file syntax**

Run: `php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`

Expected: No syntax errors

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "fix: add PHPDoc annotations to stub methods (SonarCloud S1186, S1172)"
```

---

## Task 7: Fix benchmarks — empty catch blocks (S108)

**Files:**
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php` (lines 70, 125, 193, 220)
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php` (lines 78, 105, 134, 192, 198, 204, 210)
- Modify: `benchmarks/src/Drivers/BunnyDriver.php` (lines 52, 184)
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php` (line 79)
- Modify: `benchmarks/laravel/LaravelCompareBenchmark.php` (lines 47, 83)
- Modify: `benchmarks/laravel/LaravelSmokeBenchmark.php` (line 86)
- Modify: `crates/rabbit-rs-php/tests/Publisher/BackpressureTest.php` (line 47)
- Modify: `crates/rabbit-rs-php/tests/Pool/CallbackDeadlockTest.php` (line 55)

**Interfaces:**
- Consumes: None
- Produces: All empty catch blocks have explanatory comments

- [ ] **Step 1: Add comments to empty catch blocks**

For each `catch (\Throwable) {}` or `catch (\Exception) {}` block, add a comment explaining why the exception is intentionally swallowed. These are best-effort cleanup/teardown operations where the exception is not actionable.

Pattern to use:
```php
} catch (\Throwable) {
    // Best-effort: ignore errors during cleanup/teardown.
}
```

For purgeQueue catches:
```php
} catch (\Throwable) {
    // Queue may not exist yet; safe to ignore.
}
```

For connection disconnect catches:
```php
} catch (\Throwable) {
    // Connection may already be closed; safe to ignore.
}
```

- [ ] **Step 2: Commit**

```bash
git add benchmarks/ crates/rabbit-rs-php/tests/
git commit -m "fix: add explanatory comments to empty catch blocks (SonarCloud S108)"
```

---

## Task 8: Fix benchmarks — dedicated exceptions instead of RuntimeException (S112)

**Files:**
- Modify: `benchmarks/src/Budget.php` (lines 16, 20)
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php` (lines 27, 78, 133)
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php` (lines 86, 129)
- Modify: `benchmarks/src/Drivers/BunnyDriver.php` (lines 60, 128)
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php` (lines 87, 129)
- Modify: `benchmarks/laravel/LaravelCompareBenchmark.php` (line 148)
- Modify: `benchmarks/laravel/LaravelSmokeBenchmark.php` (line 29)
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php` (line 129)
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php` (line 122)
- Modify: `packages/laravel-queue/src/Support/NativePoolFactory.php` (line 36)
- Modify: `crates/rabbit-rs-php/tests/Pool/ForkInvalidationTest.php` (line 33)

**Interfaces:**
- Consumes: None
- Produces: Dedicated exception classes instead of generic RuntimeException/Exception

- [ ] **Step 1: Create a dedicated benchmark exception**

Create: `benchmarks/src/BenchmarkException.php`
```php
<?php

declare(strict_types=1);

namespace Bench;

class BenchmarkException extends \RuntimeException
{
}
```

- [ ] **Step 2: Create a dedicated test exception for extension tests**

The extension test files throw `\RuntimeException`. Since these are test files, the simplest approach is to create a test-specific exception or use the existing `\Goopil\RabbitRs\Exception` class.

For `crates/rabbit-rs-php/tests/Pool/ForkInvalidationTest.php`, replace `throw new \RuntimeException(...)` with `throw new \Goopil\RabbitRs\Exception(...)` since that's the extension's own exception class.

- [ ] **Step 3: Replace RuntimeException in benchmark source files**

In all benchmark files listed above, replace:
- `throw new \RuntimeException(...)` → `throw new BenchmarkException(...)`
- `throw new RuntimeException(...)` → `throw new BenchmarkException(...)`

Add `use Bench\BenchmarkException;` import where needed.

For benchmark driver constructor checks (e.g. "The pecl 'amqp' extension is not loaded"), use `BenchmarkException`.

- [ ] **Step 4: Replace RuntimeException in Laravel package source**

For `WorkerSupervisor.php:129`: the `run()` method throws `\RuntimeException` when ext-pcntl is missing. Create or use an existing dedicated exception.

Check if `QueueException` exists and is appropriate. If not, create `packages/laravel-queue/src/Exceptions/SupervisorException.php`:
```php
<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

class SupervisorException extends \RuntimeException
{
}
```

For `RabbitMqServiceProvider.php:122`: `throwMissingNativeExtension()` throws `RuntimeException`. Create or use `QueueException` — but this is about the extension, not the queue. Create `packages/laravel-queue/src/Exceptions/MissingExtensionException.php`:
```php
<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Exceptions;

class MissingExtensionException extends \RuntimeException
{
}
```

For `NativePoolFactory.php:36`: throws `RuntimeException` when getmypid() fails. Use `QueueException` or create `PoolException`. Since the existing `QueueException` is in the same namespace, check if it fits. If not, create `PoolException`.

- [ ] **Step 5: Replace RuntimeException in test files**

For test files that throw generic exceptions as part of test assertions (e.g., `RabbitMqStatusCommandTest.php`), use a test-specific exception class or `\Exception` subclass. Since SonarCloud flags `throw new \Exception(...)`, create a simple test exception or use `RuntimeException` from a test namespace.

For `packages/laravel-queue/tests/Unit/Console/RabbitMqStatusCommandTest.php` (lines 11, 30): These throw `\Exception` to test error handling. Create a test-specific exception:
```php
// At the top of the test file
class TestException extends \Exception {}
```
Then use `throw new TestException(...)` instead of `throw new \Exception(...)`.

Same pattern for `packages/laravel-queue/tests/Unit/NativePoolFactoryTest.php:33`.

- [ ] **Step 6: Run tests to verify**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --testdox`

Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add benchmarks/ packages/laravel-queue/ crates/rabbit-rs-php/tests/
git commit -m "fix: use dedicated exceptions instead of generic RuntimeException (SonarCloud S112)"
```

---

## Task 9: Fix benchmarks — unused parameters and variables (S1172, S1481, S1854)

**Files:**
- Modify: `benchmarks/src/Drivers/BunnyDriver.php` (line 111: unused `$expected`)
- Modify: `packages/laravel-queue/src/Horizon/RabbitMqQueue.php` (line 36: unused `$messageId`)
- Modify: `packages/laravel-queue/src/Console/RabbitMqWorkCommandExtension.php` (line 133: unused `$event`)
- Modify: `packages/laravel-queue/tests/bootstrap.php` (line 39: unused `$job`)
- Modify: `packages/laravel-queue/tests/Unit/Console/RabbitMqStatusCommandTest.php` (lines 10, 29: unused `$config`)
- Modify: `packages/laravel-queue/tests/Unit/NativePoolFactoryTest.php` (lines 36, 149: unused `$config`; lines 98, 135, 160: useless `$processId` assignment)
- Modify: `packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php` (line 116: unused `$level`, `$context`)
- Modify: `packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php` (lines 35, 49, 65, 79: unused `$queue`)
- Modify: `packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php` (lines 216, 226, 236: unused `$queue`)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqQueueCleanupTest.php` (line 110: unused `$pool`)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqQueuePopTest.php` (line 80: unused `$pool`)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php` (line 77: useless `$processId` assignment)
- Modify: `crates/rabbit-rs-php/tests/Consumer/FireAndForgetTest.php` (line 136: unused `$first`)
- Modify: `crates/rabbit-rs-php/tests/Publisher/BackpressureTest.php` (line 27: unused `$consumer`)
- Modify: `crates/rabbit-rs-php/tests/Pest.php` (line 12: redundant jump)

**Interfaces:**
- Consumes: None
- Produces: All unused parameters removed or annotated, unused variables removed

- [ ] **Step 1: Fix unused function parameters**

For `BunnyDriver.php:111`: `waitForConfirms(int $expected)` — the parameter is used on line 113 (`$targetSeq = $this->publishSeq`). Check if it's truly unused or if SonarCloud is wrong. If the method uses `$targetSeq` instead of `$expected`, the parameter may be redundant. Remove it if not needed, or use it.

For `RabbitMqQueue.php` (Horizon): `pushRaw` callback at line 36 — `$messageId` is the return of `tap()`. If unused, rename to `$_` or remove the closure parameter.

For `RabbitMqWorkCommandExtension.php:133`: The `WorkerIdle` event handler receives `$event` but doesn't use it. Change to `static function () use (...)` without the `$event` parameter. But since it's an event listener, the parameter is required by the event system. Add `/** @noinspection PhpUnusedParameterInspection */`.

For `bootstrap.php:39`: unused `$job` parameter in a closure. Remove it or prefix with `_`.

For `RabbitMqStatusCommandTest.php:10, 29`: unused `$config` parameters in test methods. Remove or prefix with `_`.

For `NativePoolFactoryTest.php:36, 149`: unused `$config` — remove or prefix.

For `RabbitMqWorkCommandTest.php:116`: unused `$level` and `$context` in a test logger callback. Remove or prefix.

- [ ] **Step 2: Fix unused local variables (S1481)**

Remove unused variable assignments in test files:
- `NativeEventDispatchTest.php:35,49,65,79`: `$queue = ...` — remove the assignment
- `OctaneLifecycleTest.php:216,226,236`: `$queue = ...` — remove
- `RabbitMqQueueCleanupTest.php:110`: `$pool = ...` — remove
- `RabbitMqQueuePopTest.php:80`: `$pool = ...` — remove
- `FireAndForgetTest.php:136`: `$first = ...` — remove
- `BackpressureTest.php:27`: `$consumer = ...` — remove

- [ ] **Step 3: Fix useless assignments (S1854)**

- `NativePoolFactoryTest.php:98, 135, 160`: `$processId = ...` where the value is never used. Remove the assignment.
- `RabbitMqConnectorTest.php:77`: Same pattern — remove the assignment.

- [ ] **Step 4: Fix redundant jump (S3626)**

`Pest.php:12`: Remove the redundant `return` statement.

- [ ] **Step 5: Run tests**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --testdox`

Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add benchmarks/ packages/laravel-queue/ crates/rabbit-rs-php/tests/
git commit -m "fix: remove unused parameters, variables, and dead code (SonarCloud S1172, S1481, S1854, S3626)"
```

---

## Task 10: Fix duplicated string literals (S1192)

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php` (lines 74, 174, 220, 267, 316)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php` (line 155)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqJobTest.php` (line 40)
- Modify: `packages/laravel-queue/tests/Unit/HorizonRabbitMqJobTest.php` (line 16)
- Modify: `packages/laravel-queue/tests/Unit/HorizonRabbitMqQueueTest.php` (line 64)
- Modify: `packages/laravel-queue/tests/Unit/MessageMapperTest.php` (line 22)
- Modify: `packages/laravel-queue/tests/Integration/AtLeastOnceChaosTest.php` (line 76)
- Modify: `packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php` (line 12)
- Modify: `packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php` (line 17)
- Modify: `packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php` (line 81)
- Modify: `packages/laravel-queue/tests/Fixture/worker_stub_functions.php` (line 19)
- Modify: `packages/laravel-queue/tests/Unit/Console/RabbitMqStatusCommandTest.php` (line 19)

**Interfaces:**
- Consumes: None
- Produces: All duplicated string literals (3+ occurrences) extracted to constants

- [ ] **Step 1: Fix ConfigNormalizer.php — extract validation message constants**

Add class-level constants for the most duplicated strings:

```php
private const MSG_MUST_BE_ARRAY = 'must be an array';
private const MSG_MUST_BE_NULL_OR_STRING = 'must be null or a string';
private const MSG_NO_ACK = '.no_ack';
private const MSG_BROKER = '.broker';
private const MSG_SUBSCRIPTIONS = '.subscriptions';
```

Then replace all occurrences of these string literals in the file with the constants.

Specifically:
- Line 74: `'must be an array'` appears 13 times — replace all with `self::MSG_MUST_BE_ARRAY`
- Line 174: `'must be null or a string'` appears 3 times — replace with `self::MSG_MUST_BE_NULL_OR_STRING`
- Line 220: `'.broker'` appears 4 times — replace with `self::MSG_BROKER`
- Line 267: `'.subscriptions'` appears 3 times — replace with `self::MSG_SUBSCRIPTIONS`
- Line 316: `'.no_ack'` appears 3 times — replace with `self::MSG_NO_ACK`

- [ ] **Step 2: Fix RabbitMqQueue.php — extract content type constant**

Line 155: `'application/json'` appears 4 times. Add:
```php
private const CONTENT_TYPE_JSON = 'application/json';
```
Replace all 4 occurrences in `push()`, `later()`, `laterRawFromPayload()`, and `prepareBatch()`.

- [ ] **Step 3: Fix test file duplicate strings**

For each test file, add constants at the class/file level:

- `RabbitMqJobTest.php:40`: UUID `'018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137'` (5×) → `private const TEST_MESSAGE_ID = '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137';`
- `HorizonRabbitMqJobTest.php:16`: same UUID (3×) → same constant

> **⚠️ FIXED DURING EXECUTION (2026-08-29, commit eca073e):** `private const` is invalid
> at the Pest file level, and declaring the SAME name `TEST_MESSAGE_ID` in two files
> loaded in the same Pest process produces "Constant already defined" warnings.
> Delivered resolution: constants renamed per file — `RABBIT_MQ_JOB_TEST_MESSAGE_ID`
> (RabbitMqJobTest) and `HORIZON_RABBIT_MQ_JOB_TEST_MESSAGE_ID` (HorizonRabbitMqJobTest),
> plain `const` (without `private`). Do not re-execute the prescription verbatim above.
- `HorizonRabbitMqQueueTest.php:64`: `'Laravel\Horizon\Events\'` (4×) → `private const HORIZON_EVENTS_NS = 'Laravel\Horizon\Events\';`
- `MessageMapperTest.php:22`: `'{"job":"App\\Jobs\\Example"}'` (3×) → constant
- `AtLeastOnceChaosTest.php:76`: `'rabbit@rabbitmq-1'` (3×) → constant
- `RabbitMqWorkCommandTest.php:12`: `'Illuminate\Contracts\Console\Kernel'` (7×) → constant
- `RabbitMqStatusCommandTest.php:17`: `'rabbit-rs:status --format=json'` (5×) → constant
- `WorkerSupervisorIntegrationTest.php:81`: `'/Fixture/worker_stub.php'` (3×) → constant
- `worker_stub_functions.php:19`: `'/worker-'` (3×) → constant
- `RabbitMqStatusCommandTest.php:19`: `'Failed to collect stats'` (3×) → constant

- [ ] **Step 4: Run tests**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --testdox`

Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/
git commit -m "fix: extract duplicated string literals to constants (SonarCloud S1192)"
```

---

## Task 11: Fix ConfigNormalizer — cognitive complexity (S3776)

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php` (line 239: `workers()` method, complexity 42)

**Interfaces:**
- Consumes: None
- Produces: `workers()` method refactored to reduce cognitive complexity below 15

- [ ] **Step 1: Extract subscription validation to a separate method**

The `workers()` method (lines 239-372) has complexity 42. Break it down:

Extract the subscription normalization loop body into `normalizeSubscription()`:
```php
/**
 * @return array{name: string, broker: string, queue: string, weight: int, priority_class: int, prefetch: int, starvation_after: int, early_ack: bool, no_ack: bool}
 */
private static function normalizeSubscription(
    mixed $subscription,
    string $subscriptionName,
    string $subscriptionPath,
    array $brokerNames,
    int $maxInFlight,
    bool $bestEffort,
): array {
    // Move the subscription validation and normalization logic here
}
```

Extract the `no_ack` validation into `validateNoAck()`:
```php
private static function validateNoAck(
    bool $noAck,
    bool $earlyAck,
    bool $bestEffort,
    string $subscriptionName,
    string $subscriptionPath,
): void {
    if (!$noAck) {
        return;
    }
    if (!$earlyAck) {
        self::invalid($subscriptionPath.'.no_ack', "no_ack=true requires early_ack=true for subscription '{$subscriptionName}'");
    }
    if (!$bestEffort) {
        self::invalid($subscriptionPath.'.no_ack', "no_ack=true requires best_effort=true for subscription '{$subscriptionName}'");
    }
}
```

Extract the `early_ack` validation into its own call. The key is to reduce nesting and branch count in `workers()` itself.

- [ ] **Step 2: Run tests**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --filter ConfigNormalizer --testdox`

Expected: All pass

- [ ] **Step 3: Commit**

```bash
git add packages/laravel-queue/src/Config/ConfigNormalizer.php
git commit -m "fix: reduce ConfigNormalizer::workers() cognitive complexity (SonarCloud S3776)"
```

---

## Task 12: Fix benchmark drivers — cognitive complexity (S3776)

**Files:**
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php` (lines 75: complexity 18, 130: complexity 19)
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php` (line 126: complexity 24)
- Modify: `benchmarks/src/Drivers/BunnyDriver.php` (line 125: complexity 19)
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php` (line 126: complexity 31)

**Interfaces:**
- Consumes: None
- Produces: Benchmark driver methods with cognitive complexity ≤ 15

- [ ] **Step 1: Refactor AmqpExtDriver::publishMessages (line 75, complexity 18)**

Extract the fire-and-forget loop and the confirm-mode loop into separate methods:
```php
private function publishFireAndForget(int $count): void { ... }
private function publishWithConfirms(int $count, int $batchSize): void { ... }
```

- [ ] **Step 2: Refactor AmqpExtDriver::consumeMessages (line 130, complexity 19)**

Extract the callback and the consume loop into methods:
```php
private function makeConsumeCallback(int $count, bool $autoAck, string $consumerTag): \Closure { ... }
private function consumeWithTimeouts(\AMQPQueue $queue, \Closure $callback, int $count, bool $autoAck, string $consumerTag, string $flags): void { ... }
```

- [ ] **Step 3: Refactor AmqplibDriver::consumeMessages (line 126, complexity 24)**

Same pattern: extract callback creation and the consume loop.

- [ ] **Step 4: Refactor BunnyDriver::consumeMessages (line 125, complexity 19)**

Extract the callback and the consume loop.

- [ ] **Step 5: Refactor RabbitRsDriver::consumeMessages (line 126, complexity 31)**

This is the most complex. Split the BATCH_CONFIRM path and the normal path into separate methods:
```php
private function consumeBatchConfirm(int $count): void { ... }
private function consumeSingle(int $count): void { ... }
```

- [ ] **Step 6: Commit**

```bash
git add benchmarks/src/Drivers/
git commit -m "fix: reduce cognitive complexity in benchmark drivers (SonarCloud S3776)"
```

---

## Task 13: Fix RabbitMqQueue — too many methods, identical methods (S1448, S4144)

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php` (S1448: 28 methods)
- Modify: `packages/laravel-queue/src/Octane/OctaneLifecycle.php` (S4144: `stop()` identical to `reload()`)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqServiceProviderTest.php` (S4144: identical methods)

**Interfaces:**
- Consumes: None
- Produces: RabbitMqQueue with suppression annotation, OctaneLifecycle with differentiated methods

- [ ] **Step 1: Add suppression annotation to RabbitMqQueue**

Since this is a performance-critical hot path and the class implements Laravel's Queue contract (which dictates the method count), add a PHPDoc suppression:

At the class level (line 24), add:
```php
/**
 * @noinspection PhpTooManyMethodsInspection
 * @phpstan-ignore-next-line
 *
 * Method count is dictated by the Illuminate\Contracts\Queue\Queue interface
 * and Laravel's Queue base class. Splitting would add indirection on the hot path.
 */
class RabbitMqQueue extends Queue implements QueueContract
```

- [ ] **Step 2: Fix OctaneLifecycle — differentiate stop() from reload()**

The `stop()` method (line 40) is identical to `reload()` (line 31). While they do the same thing now, they have different semantics. Add a comment to `stop()` explaining the intentional duplication:

```php
/**
 * Called when the Octane worker stops. All pools are flushed.
 *
 * This intentionally mirrors {@see reload()} — both operations require
 * a full flush, but stop() may diverge in the future (e.g. waiting for
 * in-flight work before flushing).
 */
public function stop(): void
{
    $this->closeConsumersOnResolvedQueues();
    $this->flushPoolFactory();
}
```

- [ ] **Step 3: Fix identical test methods in RabbitMqServiceProviderTest**

Line 44 has a method identical to `getCachedConfigPath()` on line 39. Merge them or differentiate. Check the actual code and either:
- Remove the duplicate method and use the original
- Add a different implementation if the test requires it

- [ ] **Step 4: Run tests**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit --testdox`

Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/
git commit -m "fix: suppress RabbitMqQueue method count, differentiate OctaneLifecycle methods (SonarCloud S1448, S4144)"
```

---

## Task 14: Fix useless object instantiations and require_once (S1848, S2003)

**Files:**
- Modify: `crates/rabbit-rs-php/tests/Config/ConfigValidationTest.php` (lines 53, 67, 80, 92)
- Modify: `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php` (line 173)
- Modify: `crates/rabbit-rs-php/tests/Extension/SecretsTest.php` (lines 14, 33)
- Modify: `benchmarks/src/run-benchmarks.php` (lines 19, 27)
- Modify: `packages/laravel-queue/tests/bootstrap.php` (line 6)

**Interfaces:**
- Consumes: None
- Produces: Object instantiations are used or removed, require→require_once

- [ ] **Step 1: Fix useless Pool instantiations in test files**

In `ConfigValidationTest.php`, `ReflectionTest.php`, and `SecretsTest.php`, `\Goopil\RabbitRs\Pool` is instantiated but the result is not used. These are likely testing that the constructor doesn't throw. Fix by either:
- Assigning to a variable: `$pool = new Pool($config);` and adding an assertion like `expect($pool)->toBeInstanceOf(Pool::class)`
- Or if the test is about constructor validation, wrap in a try/catch or use `expect(fn() => new Pool($config))->toThrow(...)`

Check each test's intent and fix accordingly.

- [ ] **Step 2: Fix require→require_once**

In `benchmarks/src/run-benchmarks.php` lines 19, 27: change `require` to `require_once`.
In `packages/laravel-queue/tests/bootstrap.php` line 6: change `require` to `require_once`.

- [ ] **Step 3: Commit**

```bash
git add crates/rabbit-rs-php/tests/ benchmarks/src/run-benchmarks.php packages/laravel-queue/tests/bootstrap.php
git commit -m "fix: use require_once and fix useless object instantiations (SonarCloud S1848, S2003)"
```

---

## Task 14b: Switch SonarCloud target to the autoscan project `Goopil_php-rabbit-rs`

> **ADDED 2026-08-29** — consequence of the GitHub repo rename to
> `Goopil/php-rabbit-rs` + user decision: switch to the autoscan project.
> Autoscan and CI analysis being mutually exclusive, the Sonar CI job disappears.

**Files:**
- Modify: `sonar-project.properties` (key + links)
- Modify: `.github/workflows/coverage.yml` (removal of the `sonarcloud` job + dedicated artifact steps)

**Interfaces:**
- Consumes: user decision (autoscan switch)
- Produces: SonarCloud fed only by autoscan; coverage only toward Codecov

- [ ] **Step 1: `sonar-project.properties`**

`sonar.projectKey=Goopil_php-rabbit-rs`. Update the 3 `sonar.links.*` to
`https://github.com/Goopil/php-rabbit-rs`. Remove the 2 coverage path lines
(`sonar.coverage.lcovReportPaths`, `sonar.php.coverage.reportPaths`):
dead with autoscan (it does not consume coverage reports). Keep
sources/tests/exclusions (autoscan reads these properties).

- [ ] **Step 2: `coverage.yml` — remove the `sonarcloud` job**

Delete the whole `sonarcloud` job (checkout fetch-depth 0, artifact downloads,
placement, scan). Also delete the 3 "Upload artifact" steps
(`coverage-rust`, `coverage-php-ext`, `coverage-laravel`) that existed only to
feed that job — verify no other step consumes these artifacts before
removing. The Codecov uploads remain unchanged.

- [ ] **Step 3: Verify**

Valid YAML (`python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/coverage.yml'))"`,
or actionlint if installed). `grep -rn "Goopil_rabbit-rs\|sonarqube-scan\|download-artifact" .github/workflows/coverage.yml sonar-project.properties`
must return nothing. No other `Goopil_rabbit-rs` reference outside docs
(the docs URLs `Goopil/rabbit-rs` are redirected by GitHub — out of scope).

- [ ] **Step 4: Commit**

```bash
git add sonar-project.properties .github/workflows/coverage.yml
git commit -m "ci: switch SonarCloud target to autoscan project Goopil_php-rabbit-rs"
```

---

## Task 15: Fix accessibility bypass and nested ternary (S3011, S3358)

**Files:**
- Modify: `packages/laravel-queue/tests/Feature/OctaneLifecycleHooksTest.php` (lines 56, 57)
- Modify: `packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php` (line 64)
- Modify: `packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php` (line 14)
- Modify: `packages/laravel-queue/tests/Fixture/worker_stub.php` (lines 31, 33, 36, 38)

**Interfaces:**
- Consumes: None
- Produces: Accessibility bypasses marked as safe, nested ternaries extracted

- [ ] **Step 1: Mark accessibility bypasses as safe (S3011)**

These use reflection to access private/protected properties in tests. Add `/** @noinspection PhpUndefinedClassInspection */` or a comment:

```php
// @phpstan-ignore-next-line — intentionally accessing private property for test verification.
```

Or use `@phpstan-ignore-line` on the specific line.

- [ ] **Step 2: Extract nested ternaries in worker_stub.php (S3358)**

Lines 31, 33, 36, 38 have nested ternary operations. Refactor to if/else or separate statements:

Example for line 31:
```php
// Before
$value = $a ? $b : ($c ? $d : $e);
// After
if ($a) {
    $value = $b;
} elseif ($c) {
    $value = $d;
} else {
    $value = $e;
}
```

Read the file to see the actual ternaries and refactor each one.

- [ ] **Step 3: Commit**

```bash
git add packages/laravel-queue/tests/
git commit -m "fix: mark test accessibility bypasses and extract nested ternaries (SonarCloud S3011, S3358)"
```

---

## Task 16: Fix Dockerfile issues (S6506, S6471, S7018, S7020, S7026, S6471)

**Files:**
- Modify: `examples/laravel/Dockerfile` (lines 15, 25, 44)
- Modify: `lab/rabbitmq/rabbitmq/Dockerfile` (lines 1, 8, 9)
- Modify: `lab/rabbitmq/rabbitmq/Dockerfile.no-plugin` (line 1)

**Interfaces:**
- Consumes: None
- Produces: Dockerfiles with sorted packages, HTTPS enforcement, user safety comments

- [ ] **Step 1: Sort package names in examples/laravel/Dockerfile (S7018)**

Line 15: sort the `apt-get install` packages alphabetically:
```dockerfile
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    libzip-dev \
    unzip \
    && rm -rf /var/lib/apt/lists/*
```

- [ ] **Step 2: Fix curl HTTPS in examples/laravel/Dockerfile (S6506)**

Line 25: The `curl -L https://github.com/...` follows redirects. Add `--retry 3` and a comment noting HTTPS is enforced:
```dockerfile
RUN curl -L --retry 3 https://github.com/php/pie/releases/latest/download/pie.phar -o /usr/local/bin/pie \
    && chmod +x /usr/local/bin/pie
```

- [ ] **Step 3: Replace curl with ADD in examples/laravel/Dockerfile (S7026)**

Line 25: Replace the `curl` + `RUN` with `ADD`:
```dockerfile
ADD https://github.com/php/pie/releases/latest/download/pie.phar /usr/local/bin/pie
RUN chmod +x /usr/local/bin/pie
```

- [ ] **Step 4: Add USER safety comments for root user (S6471)**

For `examples/laravel/Dockerfile:44`, `lab/rabbitmq/rabbitmq/Dockerfile:1`, `Dockerfile.no-plugin:1`:
Add comments explaining why running as root is safe in these contexts:
```dockerfile
# Running as root is safe: this is a development lab image, not production.
FROM php:8.4-cli AS production
```

- [ ] **Step 5: Fix HTTPS in lab Dockerfile (S6506)**

Line 8: The `curl` command downloads the plugin. Ensure it uses HTTPS (it already does). Add a comment:
```dockerfile
# HTTPS enforced via curl -fsSL
&& curl -fsSL -o "..." "${PLUGIN_URL}" \
```

- [ ] **Step 6: Fix long line in lab Dockerfile (S7020)**

Line 9: Split the long `sha256sum` line with backslash continuation.

- [ ] **Step 7: Commit**

```bash
git add examples/laravel/Dockerfile lab/rabbitmq/rabbitmq/
git commit -m "fix: Dockerfile quality issues (SonarCloud S6506, S6471, S7018, S7020, S7026)"
```

---

## Task 17: Fix hardcoded credentials and AMQP protocol (S2068, S5332)

**Files:**
- Modify: `benchmarks/src/Config.php` (line 24)
- Modify: `lab/rabbitmq/rabbitmq/definitions.json` (line 10)
- Modify: `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php` (line 167)

**Interfaces:**
- Consumes: None
- Produces: Credentials documented as safe, AMQP test connection documented

- [ ] **Step 1: Document benchmark credentials as safe**

In `benchmarks/src/Config.php`, add a class-level PHPDoc comment:
```php
/**
 * Benchmark configuration.
 *
 * @noinspection PhpUnnecessaryFullyQualifiedNameInspection
 *
 * Credentials are local lab-only values (rabbit_rs_lab). They are NOT production
 * secrets. SonarCloud S2068 is a false positive for this context.
 */
class Config
```

- [ ] **Step 2: Document lab definitions.json credentials**

In `lab/rabbitmq/rabbitmq/definitions.json`, the passwords are in a JSON file. Add a sibling file or document in the Dockerfile:
```dockerfile
# Credentials in definitions.json are local lab-only (rabbit_rs_lab, admin_lab).
# NOT production secrets — S2068 is a false positive for this context.
```

Or add `sonar-project.properties` exclusion for this file:
```
sonar.exclusions=lab/rabbitmq/rabbitmq/definitions.json
```

- [ ] **Step 3: Fix AMQP protocol in ReflectionTest (S5332)**

Line 167: The test uses `amqp://` instead of `amqps://`. This is a test file connecting to a local lab broker. Add a comment:
```php
// amqp:// is safe here: connecting to a local lab broker without TLS for testing.
$connection = 'amqp://...';
```

Or change to `amqps://` if the test can work with TLS.

- [ ] **Step 4: Commit**

```bash
git add benchmarks/src/Config.php lab/rabbitmq/rabbitmq/ crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php sonar-project.properties
git commit -m "fix: document lab credentials as safe, fix AMQP protocol (SonarCloud S2068, S5332)"
```

---

## Task 18: Fix remaining misc issues — regex, function length, identical methods (S6353, S138, S4144)

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php` (line 130: `[0-9]` → `\d`)
- Modify: `packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php` (line 10: function too long, 169 lines)

**Interfaces:**
- Consumes: None
- Produces: Concise regex, shorter test functions

- [ ] **Step 1: Fix regex character class (S6353)**

In `ConfigNormalizer.php:130`, change `[0-9]` to `\d` in the preg_match pattern:
```php
// Before
if (preg_match('/^\[([^]]+)](?::([0-9]+))?$/', $endpoint, $matches) !== 1) {
// After
if (preg_match('/^\[([^]]+)](?::(\d+))?$/', $endpoint, $matches) !== 1) {
```

- [ ] **Step 2: Fix function length in RabbitMqWorkCommandTest (S138)**

Line 10: A function expression has 169 lines (limit 150). Extract setup helper methods:
```php
private function defaultConfig(): array { ... }
private function assertWorkerProcesses(callable $callback): void { ... }
```

Split the test into smaller functions.

- [ ] **Step 3: Commit**

```bash
git add packages/laravel-queue/src/Config/ConfigNormalizer.php packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
git commit -m "fix: regex conciseness and function length (SonarCloud S6353, S138)"
```

---

## Task 19: Run full quality gate and verify SonarCloud clean

**Files:**
- None (verification only)

**Interfaces:**
- Consumes: All previous tasks
- Produces: Verified clean SonarCloud report

- [ ] **Step 1: Run Rust quality gate**

Run: `rtk cargo fmt --all -- --check && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo nextest run --workspace --all-targets --no-fail-fast`

Expected: All pass

- [ ] **Step 2: Run PHP tests**

Run: `rtk ./scripts/test-laravel.sh`

Expected: All pass

- [ ] **Step 3: Run full quality gate**

Run: `rtk ./scripts/check.sh`

Expected: All pass

- [ ] **Step 4: SonarCloud verification (via autoscan — AMENDED 2026-08-29)**

The branch is analyzed automatically by the `Goopil_php-rabbit-rs` autoscan
(branch push = analysis + PR decoration). Verify the
sonarqubecloud comment on the MR (new issues on the changed code) — the Sonar CI
job no longer exists (Task 14b).

- [ ] **Step 5: Verify SonarCloud issues cleared**

Check: `https://sonarcloud.io/project/issues?id=Goopil_php-rabbit-rs`

Expected: after merge to main (autoscan re-scan), the total count of the 204 reference issues
drops accordingly; the rules targeted by the plan no longer generate
issues on new code. The expected residues (inert @noinspection pattern:
S1448 RabbitMqQueue, S4144 OctaneLifecycle, S1172 WorkerIdle, stubs
S1186/S1172) are covered by the dedicated arbitration (NOSONAR vs UI won't-fix).
