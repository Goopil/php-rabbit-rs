# ext-php-rs 0.15.15 → 0.16.3 Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `crates/rabbit-rs-php` from the exact-pinned `ext-php-rs = "=0.15.15"` to `=0.16.3` (issue #359), clearing the RUSTSEC-2026-0367/-0368/-0369 deny ignores (zip < 9) and enabling the follow-up CI runner unpin.

**Architecture:** Compile-first migration: bump the pin, fix the single hard API break (the pending-exception path in `callbacks.rs`), then walk the verification ladder (Rust harness → supply chain → stubs → extension suites → CI unpin). The 0.16 error model moves a caught PHP exception from the `Error` value into the engine globals (`Error::ExceptionPending { class: String }` + `ExecutorGlobals::take_exception() -> Option<ZBox<ZendObject>>`), so our rethrow path must take ownership from the globals instead of unpacking the error payload.

**Tech Stack:** Rust 1.98.1 (pinned), ext-php-rs 0.16.3 (upstream bindgen 0.73 — clang 21 compatible), cargo-php ≥ 0.2 (required by the 0.16 introspection ABI), Pest/PHPT for the extension suites.

## Global Constraints

- Unsafe Rust is forbidden: keep `#![forbid(unsafe_code)]` intact. The fix below uses only safe API (`ExecutorGlobals::take_exception`, `IntoZval::set_zval`).
- `ext-php-rs` stays exact-pinned: `=0.16.3` (current style is `=0.15.15`).
- `rabbit-rs-core` does not depend on ext-php-rs — no core changes.
- All repository artifacts in English.
- Do not weaken or bypass `scripts/check.sh`; run `rtk cargo fmt --all` after Rust edits.
- Never retain Zend values in Rust threads (the taken `ZBox<ZendObject>` is consumed synchronously on the PHP thread).
- The extension is never installed system-wide during dev; suites load from `target/debug|release` via `scripts/lib-extension.sh`.

## Evidence Summary (from the 0.16 migration guide vs. our crate)

- 34 breaking changes reviewed: **one hard compile break** (`callbacks.rs:100`), one cosmetic (`bridge.rs:118`), one supply-chain task (deny ignores), no feature/Cargo.toml changes (we enable defaults only), no stub-signature changes expected.
- Three runtime risks to verify, not assume: (1) cargo-php must be ≥ 0.2 to read the new `ext_php_rs_describe_module_v2` describe pointer; (2) the `zend-link-stubs` symbol set may grow; (3) the no-engine raise path must still panic (two Rust harnesses rely on it: `publish_buffer_state_machine.rs:68-76`, `publish_buffer.rs:1078`).

---

### Task 1: Bump the pin and fix the pending-exception break

**Files:**
- Modify: `crates/rabbit-rs-php/Cargo.toml:25`
- Modify: `crates/rabbit-rs-php/src/callbacks.rs:11-16` (imports) and `:96-125` (match arm)
- Modify: `crates/rabbit-rs-php/src/classes/bridge.rs:118`

**Interfaces:**
- Consumes: `ext_php_rs::error::Error::ExceptionPending { class: String }` (0.16.3 shape), `ext_php_rs::zend::ExecutorGlobals::take_exception() -> Option<ZBox<ZendObject>>` (0.16.3 `src/zend/globals.rs:174`), `IntoZval for ZBox<ZendObject>` (`set_zval(self, zv: &mut Zval, persistent: bool) -> Result<()>`, 0.16.3 `src/types/object.rs:792`).
- Produces: unchanged public PHP surface (event bridge still rethrows the callback's exception object as-is). No signatures change.

- [ ] **Step 1: Bump the pin**

```toml
ext-php-rs = "=0.16.3"
```

- [ ] **Step 2: Observe the expected compile failure**

Run: `rtk cargo check -p rabbit-rs-php`
Expected: FAIL — `no variant or associated item named 'Exception' found for enum 'Error'` at `src/callbacks.rs:100` (and possibly an unused-must-use note at `bridge.rs:118`).

- [ ] **Step 3: Fix the pending-exception path**

In `src/callbacks.rs`, extend the `ext_php_rs` import block with `zend::ExecutorGlobals`:

```rust
use ext_php_rs::{
    convert::{IntoZval, IntoZvalDyn},
    error::Error,
    prelude::{PhpException, PhpResult},
    types::{ZendCallable, Zval},
    zend::ExecutorGlobals,
};
```

Replace the arm at `src/callbacks.rs:100-119`:

```rust
                // In ext-php-rs 0.16 the caught exception object lives in the
                // engine globals, not in the error payload: take ownership of
                // it so it is rethrown as-is instead of being stringified and
                // destroyed.
                Err(Error::ExceptionPending { .. }) => match ExecutorGlobals::take_exception() {
                    Some(exception) => {
                        let mut object = Zval::new();
                        if exception.set_zval(&mut object, false).is_ok() {
                            let mut thrown = crate::classes::exception::rabbit_exception_message(
                                "registered callback threw an exception".to_owned(),
                            );
                            thrown.set_object(Some(object));
                            error.get_or_insert(thrown);
                        } else {
                            // The exception object could not be converted into
                            // a zval: a fallback message keeps the failure
                            // visible instead of silently dropping the callback
                            // exception.
                            error.get_or_insert_with(|| {
                                crate::classes::exception::rabbit_exception_message(
                                    "the event callback threw but its exception could not be converted"
                                        .to_owned(),
                                )
                            });
                        }
                    }
                    None => {
                        // The pending error carried no retrievable exception
                        // object; keep the failure visible.
                        error.get_or_insert_with(|| {
                            crate::classes::exception::rabbit_exception_message(
                                "registered callback threw an exception".to_owned(),
                            )
                        });
                    }
                },
```

In `src/classes/bridge.rs:118`, replace `let _ = exception.throw();` with:

```rust
        // 0.16 makes throw() return () and a no-op when an exception is
        // already pending (audit F-17 context still applies).
        exception.throw();
```

- [ ] **Step 4: Verify the crate compiles and unit tests pass**

Run: `rtk cargo check -p rabbit-rs-php && rtk cargo test -p rabbit-rs-php`
Expected: check green; tests compile and run (loader/linker issues are Task 2's scope — if a test binary fails with `undefined symbol`, stop and go to Task 2).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-php/Cargo.toml Cargo.lock crates/rabbit-rs-php/src/callbacks.rs crates/rabbit-rs-php/src/classes/bridge.rs
git commit -m "feat(php-ext): Migrate to ext-php-rs 0.16.3

Bump the exact pin from 0.15.15. The single breaking change we hit is
the error model: a caught PHP exception moved from the Error payload
into the engine globals, so the event-bridge rethrow path now takes
ownership via ExecutorGlobals::take_exception and converts the object
through IntoZval, keeping the existing fallback messages. throw() is
no longer fallible. Clears the path to drop the RUSTSEC zip ignores
and the ubuntu-24.04 CI pins (upstream bindgen 0.73)."
```

---

### Task 2: Adapt the Rust test harness (link stubs + panic paths)

**Files:**
- Modify (if the loader names missing symbols): `crates/rabbit-rs-php/zend-link-stubs/src/lib.rs:26-52`
- Verify only: `crates/rabbit-rs-php/src/classes/publish_buffer_state_machine.rs:68-76`, `crates/rabbit-rs-php/src/classes/publish_buffer.rs:1078`

**Interfaces:**
- Consumes: the append-on-loader-error procedure documented in `zend-link-stubs/src/lib.rs:13-15`.
- Produces: a `cargo test -p rabbit-rs-php` suite that links and passes against 0.16.3.

- [ ] **Step 1: Run the crate test suite and observe**

Run: `rtk cargo test -p rabbit-rs-php --features extension-tests`
Expected: either PASS (symbol set unchanged) or a load abort naming an unresolved strong symbol (e.g. an exception-machinery symbol shifted by the 0.16 rework).

- [ ] **Step 2: If a symbol is named, append the stub**

Add one `stub!(symbol_name);` line to `zend-link-stubs/src/lib.rs`, matching the style of the existing entries, then re-run Step 1. Repeat only while the loader names further symbols.

- [ ] **Step 3: Re-verify the no-engine panic assumptions**

Run: `rtk cargo test -p rabbit-rs-php --features extension-tests publish_buffer`
Expected: the `swallow_raise` doc test at `publish_buffer_state_machine.rs:68-76` and the `catch_unwind(AssertUnwindSafe(...))` assertion at `publish_buffer.rs:1078` still pass — the no-engine raise path must panic, not abort and not return. If it no longer panics: STOP, do not force the tests green; the two harnesses need a real redesign (raise an exception object without the engine is impossible in 0.16) — report back and reassess the test strategy.

- [ ] **Step 4: Commit (only if stubs changed)**

```bash
git add crates/rabbit-rs-php/zend-link-stubs/src/lib.rs
git commit -m "fix(php-ext): Append the 0.16 exception-machinery symbols to the link stubs"
```

---

### Task 3: Supply chain and workspace gate

**Files:**
- Modify: `deny.toml` (advisories `ignore` list)
- Verify only: `Cargo.lock`

**Interfaces:**
- Consumes: the deny.toml comment block above the `ignore` list (states the removal condition: ext-php-rs 0.16 pins zip ^9).
- Produces: `cargo deny check` green with no zip ignores; workspace gate green.

- [ ] **Step 1: Drop the zip advisory ignores**

Delete the three entries so the block reads (keep the comment updated):

```toml
# RUSTSEC-2026-0367/-0368/-0369 (zip < 9: symlink/SUID permission escapes in
# extract()) were ignored while ext-php-rs 0.15 pinned zip < 9; 0.16 pins
# zip ^9.0, so the advisories are cleared and the ignores removed
# (migration issue #359).
ignore = []
```

- [ ] **Step 2: Run cargo-deny and the full local gate**

Run: `rtk cargo deny check && rtk cargo fmt --all && rtk ./scripts/check.sh`
Expected: advisories/bans/licenses/sources all ok (note: `ext-php-rs-introspection` is a new transitive crate — licenses must allow it; if the license check flags it, add the crate's license to the `allow` list in the same commit); fmt clean; clippy `-D warnings` clean; 500+ tests pass; composer validate ok.

- [ ] **Step 3: Commit**

```bash
git add deny.toml Cargo.lock
git commit -m "chore(deps): Drop the zip advisory ignores

ext-php-rs 0.16.3 pins zip ^9.0, clearing RUSTSEC-2026-0367/-0368/
-0369 at the source; the deny ignores added for 0.15 are removed."
```

---

### Task 4: Regenerate the IDE stubs (cargo-php ≥ 0.2 required)

**Files:**
- Modify (only if the regenerated output differs): `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Verify only: `crates/rabbit-rs-php/export.macos`

**Interfaces:**
- Consumes: `./scripts/stubs.sh` (wraps `cargo php stubs` with `--manifest crates/rabbit-rs-php/Cargo.toml`).
- Produces: the authoritative stub regenerated from the Rust `///` docs; expected diff is empty or comment-only (every exported function has an explicit PHP return type; no constants/props/getters changed).

- [ ] **Step 1: Reinstall cargo-php with 0.2 support**

```bash
RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php --force
cargo php --version
```

- [ ] **Step 2: Regenerate and validate**

```bash
./scripts/stubs.sh -o crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git diff --stat crates/rabbit-rs-php/stubs/
```

Expected: `php -l` clean; the diff is empty or comment-only. A semantic diff is a review signal — investigate before continuing (0.16 macro output for names/defaults changed, but our defaults are all explicit).

- [ ] **Step 3: If macOS dlsym fails on the describe symbol, export it**

If `cargo php stubs` aborts resolving the module description, add `_ext_php_rs_describe_module_v2` to `crates/rabbit-rs-php/export.macos` (next to `_get_module`), rebuild, and retry Step 2.

- [ ] **Step 4: Commit (only if the stub changed)**

```bash
git add crates/rabbit-rs-php/stubs/rabbit_rs.stub.php crates/rabbit-rs-php/export.macos
git commit -m "chore(php-ext): Regenerate the IDE stubs against ext-php-rs 0.16"
```

---

### Task 5: Extension, FPM, Laravel and integration suites

**Files:**
- No source changes expected; fix-forward if a suite exposes a real regression.

**Interfaces:**
- Consumes: `scripts/lib-extension.sh` (stamps builds against `Cargo.lock` — the feature build rebuilds automatically).
- Produces: the full local verification ladder for the migrated extension.

- [ ] **Step 1: Extension suite (Pest + PHPT)**

Run: `./scripts/test-extension.sh`
Expected: green. Spot-check the callback rethrow behavioral path: `cd crates/rabbit-rs-php && pest tests` — the CallbackHardening tests must pass (they exercise the Task 1 rethrow path).

- [ ] **Step 2: FPM multi-worker pool test**

Run: `./scripts/test-fpm.sh`
Expected: green.

- [ ] **Step 3: Laravel Unit + Feature (no extension) and Integration (with extension)**

Run: `./scripts/test-laravel.sh && ./scripts/test-integration.sh`
Expected: green. Note: 0.16 lowercases error messages — no test asserts the old casing, but watch the output for any message-matching assertion.

- [ ] **Step 4: Commit any fix-forward changes, then open the PR**

Open one PR for Tasks 1-5 (branch `feat/ext-php-rs-016`). The PR's CI still runs on the `ubuntu-24.04` pins — it proves the migration, not the unpin (Task 6 is a second PR). CodSpeed runs on the PR: the FFI layer changed, so treat any regression vs. the frozen budgets as blocking.

---

### Task 6: Unpin the CI runners (second PR, after the migration PR merges)

**Files:**
- Modify: `.github/workflows/ci.yml` (jobs `rust`, `clippy-sarif`, `integration`, `octane-runtime`)
- Modify: `.github/workflows/coverage.yml` (jobs `coverage-rust`, `coverage-php-ext`, `coverage-laravel`)
- Modify: `.github/workflows/codspeed.yml` (job `benchmarks`)
- Modify: `.github/workflows/nightly.yml` (job `octane-runtime`), `soak.yml` (job `soak`), `throughput.yml` (job `throughput`), `functional-matrix.yml` (job `integration-php85`)

**Interfaces:**
- Consumes: the 12 pin sites added on 2026-10-24 (each is `runs-on: ubuntu-24.04` with an inline comment about libclang 21).
- Produces: host-side extension builds on `ubuntu-latest` (clang 21) — proof that upstream bindgen 0.73 resolves the drift that motivated the pins.

- [ ] **Step 1: Revert the 12 pins**

For each listed job, replace:

```yaml
    # libclang 21 on ubuntu-latest (26.04) breaks ext-php-rs 0.15's bindgen; unpin after the ext-php-rs 0.16 migration
    runs-on: ubuntu-24.04
```

with:

```yaml
    runs-on: ubuntu-latest
```

- [ ] **Step 2: Verify on CI, then merge**

Open PR `chore/unpin-ci-runners`. Expected: all 24 checks green on `ubuntu-latest`, including `Rust (fmt + clippy + tests)`, the three coverage jobs and CodSpeed (these host-build the extension with clang 21 + bindgen 0.73). The Docker-based jobs (PHPT, FPM) are unaffected and must stay green as well.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/
git commit -m "ci: Unpin the host extension builds from ubuntu-24.04

ext-php-rs 0.16.3 replaced the forked bindgen with upstream 0.73,
which parses the clang 21 headers shipped on ubuntu-26.04. The pins
added on 2026-10-24 have served their purpose."
```

---

### Task 7: Close the tracker and record the round

**Files:**
- Modify: `docs/plans/ROADMAP.md` (Landed section)

**Interfaces:**
- Produces: the roadmap records the migration; #359 closes.

- [ ] **Step 1: Record in the roadmap**

Add to the `## Landed` section:

```markdown
- **ext-php-rs 0.16.3 migration (2026-10-24, issue #359)** — exact pin moved
  from 0.15.15; the event-bridge rethrow path migrated to the
  `ExceptionPending`/`take_exception` error model; zip advisory ignores
  cleared; stubs regenerated with cargo-php 0.2; host CI jobs unpinned from
  `ubuntu-24.04` (upstream bindgen 0.73 parses clang 21). Plan:
  `docs/superpowers/plans/2026-10-24-ext-php-rs-016-migration.md`.
```

- [ ] **Step 2: Close the issue**

Comment on #359 with the migration PR and the unpin PR references, then close it.
