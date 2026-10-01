# FFI Single-Copy Delivery Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce every Rust→PHP payload/header byte crossing the FFI from two memcpys to exactly one, with no PHP-facing API change.

**Architecture:** Replace the `Binary<u8>` round-trip (Bytes→Vec copy, then Vec→zend_string copy) with a private `PhpString` newtype wrapping `ZBox<ZendStr>` whose `IntoZval` impl declares `TYPE = DataType::String, NULLABLE = false`, so the macro-generated arginfo stays byte-identical and `set_zend_string` performs the single memcpy into the final ZendMM allocation.

**Tech Stack:** Rust 1.98.1 (edition 2024), ext-php-rs 0.15.15, bytes::Bytes, PHP 8.x + Pest tests, PHPT.

## Global Constraints

- Unsafe Rust is forbidden: never weaken `#![forbid(unsafe_code)]` or workspace lints.
- All repository artifacts in English (code comments, commit messages, docs).
- Prefix cargo/git commands with `rtk` (e.g. `rtk cargo fmt --all`).
- Work only in the worktree at `.worktrees/ffi-single-copy` (branch `ffi-single-copy`).
- Do not touch files outside `crates/rabbit-rs-php/` and the stub file.
- Quality gate at the end: `rtk ./scripts/check.sh` must pass.
- Design reference: `docs/plans/2026-09-29-ffi-single-copy-design.md`.
- This is a guarded refactor (no observable behavior change): existing tests are the
  regression net; do not invent new behavior.

---

### Task 1: Single-copy conversion for Delivery payload, metadata headers, and Pool::get

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs` (imports, new type, `payload()`, `insert_header`)
- Regenerate: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (expected: no diff)

**Deferred:** the `Pool::get()`/`Pool::getMessage()` call site (pool.rs) is uncommitted
management-API work on `main`; swap it to `PhpString::from_bytes` when that work lands.

**Interfaces:**
- Consumes: ext-php-rs `Zval::set_zend_string(ZBox<ZendStr>)`, `ZendStr::new(impl AsRef<[u8]>, bool) -> ZBox<ZendStr>`, `IntoZval` (`TYPE`/`NULLABLE` consts + `set_zval`), `DataType::String`, `error::Result`.
- Produces: `crates/rabbit-rs-php/src/classes/delivery.rs` exports `pub struct PhpString` with `pub(crate) fn from_bytes(bytes: &[u8]) -> Self` and `impl IntoZval for PhpString`. (The deferred `Pool::getMessage` swap will import it via `use super::delivery::PhpString;`.)

**Verification context (read before starting):**
- Existing guard tests that MUST keep passing:
  - `crates/rabbit-rs-php/tests/Consumer/ConsumerTest.php` — "delivers binary-safe payloads with metadata", asserts `$delivery->payload() === "job\0payload\xff"`.
  - `crates/rabbit-rs-php/tests/phpt/delivery_terminal_state.phpt` — same binary-safety assertion.
  - `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php:99` — asserts `Delivery::payload()` type stays `'string'`.
- `crates/rabbit-rs-php/tests/BinaryPayload/BinaryPayloadTest.php` covers the publish
  (PHP→Rust) direction, which this task does not touch.

- [x] **Step 1: Baseline — build the extension and run the PHP suite before any change**

```bash
cd .worktrees/ffi-single-copy && ./scripts/test-extension.sh
```

Expected: build succeeds, Pest + PHPT suites pass. If the environment fails here (composer
missing, PHP version), STOP and report — do not proceed with the refactor.

- [x] **Step 2: delivery.rs — imports**

Replace the `ext_php_rs` use block (currently):

```rust
use ext_php_rs::{
    binary::Binary,
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ArrayKey, ZendHashTable, Zval},
};
```

with:

```rust
use ext_php_rs::{
    boxed::ZBox,
    convert::IntoZval,
    error::Result,
    flags::{ClassFlags, DataType},
    prelude::{PhpResult, php_class, php_impl},
    types::{ArrayKey, ZendHashTable, ZendStr, Zval},
};
```

- [x] **Step 3: delivery.rs — add the PhpString type after the `Delivery` impl block (or near the top, after imports)**

```rust
/// PHP string view of Rust bytes: exactly one memcpy.
///
/// `Binary<u8>` costs two copies (Bytes→Vec, then Vec→zend_string). This newtype
/// builds the `zend_string` directly from the byte slice and hands ownership to
/// the zval, so the bytes are copied once into the final ZendMM allocation.
/// `IntoZval::TYPE` must stay `String`/non-nullable: the `#[php_impl]` macro
/// writes it into the runtime arginfo, and a `Zval` return would downgrade the
/// generated signature to `mixed`.
pub struct PhpString(ZBox<ZendStr>);

impl PhpString {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self(ZendStr::new(bytes, false))
    }
}

impl IntoZval for PhpString {
    const TYPE: DataType = DataType::String;
    const NULLABLE: bool = false;

    fn set_zval(self, zval: &mut Zval, _persistent: bool) -> Result<()> {
        zval.set_zend_string(self.0);
        Ok(())
    }
}
```

The shipped code deviates from the original `pub(crate)` spec in one way: `PhpString` is
`pub` (delivery.rs:156), forced because a `pub fn payload()` returning a `pub(crate)` type is a
private-type-in-public-interface error under the mandated `clippy -D warnings`; the field
and constructor stay crate-private (rationale documented at delivery.rs:153-154).

- [x] **Step 4: delivery.rs — change `payload()` (currently lines 34-38)**

Replace:

```rust
    /// Returns the binary-safe delivery payload.
    pub fn payload(&self) -> PhpResult<Binary<u8>> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::payload")?;
        Ok(Binary::new(self.inner.payload.to_vec()))
    }
```

with:

```rust
    /// Returns the binary-safe delivery payload.
    pub fn payload(&self) -> PhpResult<PhpString> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::payload")?;
        Ok(PhpString::from_bytes(self.inner.payload.as_ref()))
    }
```

- [x] **Step 5: delivery.rs — change the binary header arm of `insert_header`**

Replace:

```rust
        HeaderValue::Binary(value) => table.insert(key, Binary::new(value.to_vec()))?,
```

with:

```rust
        HeaderValue::Binary(value) => table.insert(key, PhpString::from_bytes(value.as_ref()))?,
```

- [x] **Step 6: compile check**

```bash
rtk cargo test -p rabbit-rs-php
```

Expected: compiles, all tests pass (the crate's Rust unit tests do not exercise
`payload()`, so green here only proves compilation).

- [x] **Step 7: done after the deferral was unblocked** (Pool::getMessage landed on
      `feat/native-fallback-for-management-api`; swapped to `PhpString::from_bytes`,
      docblock `payload: \Ext\PhpRs\Binary` → `payload: string`, unused `Binary` import
      dropped, stub regenerated — the only stub diff is that docblock line).

- [x] **Step 8: format, lint, full Rust test run**

```bash
rtk cargo fmt --all
rtk cargo clippy -p rabbit-rs-php --all-targets --all-features -- -D warnings
rtk cargo nextest run --workspace --all-targets --no-fail-fast
```

(nextest falls back to `rtk cargo test --workspace --all-targets` if nextest is not installed.)
Expected: all green.

- [x] **Step 9: regenerate stubs and inspect the diff**

```bash
./scripts/stubs.sh -o crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
rtk git diff -- crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
```

Expected: **empty diff** — the runtime arginfo is byte-identical (both `Binary<u8>` and
`PhpString` declare `TYPE = String, NULLABLE = false`), so `payload(): string` and every
docblock stay unchanged. If the diff is not empty, STOP and report. Validate the stub with
`php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`.

- [x] **Step 10: run the extension PHP suite against the rebuilt extension**

```bash
./scripts/test-extension.sh
```

Expected: Pest + PHPT pass, including the binary-safety guards named above.

- [x] **Step 11: full quality gate**

```bash
rtk ./scripts/check.sh
```

Expected: fmt + clippy + tests + `rtk composer validate --strict` all pass.

- [x] **Step 12: commit**

```bash
rtk git add crates/rabbit-rs-php/src/classes/delivery.rs
rtk git commit -m "perf(php-ext): single-copy Rust-to-PHP payload conversion"
```

Message body (if the repo wants a body, keep it short):
"Replace the Binary<u8> Vec round-trip with a PhpString newtype building the
zend_string directly from Bytes, halving the memcpys on the delivery path.
PHP signatures are unchanged (arginfo TYPE stays String)."
