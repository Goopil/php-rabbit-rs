# FFI Single-Copy Delivery Path

- **Date:** 2026-09-29
- **Status:** Approved (design discussed with the maintainer; scope: local fix only)
- **Scope:** `crates/rabbit-rs-php` (Rust→PHP conversion sites only)

## Problem

Every byte crossing the PHP↔Rust FFI boundary costs at least one unavoidable memcpy:

- **PHP → Rust (publish):** the payload must outlive the PHP call — publications are buffered
  fire-and-forget and replayed (at-least-once contract), while PHP strings live in the
  request-bound ZendMM heap destroyed at every request shutdown. Holding a `zend_string`
  refcount past the call is unsound, and releasing one from the Tokio runtime thread is also
  unsound (ZendMM is not thread-safe in NTS builds). One copy is therefore the safe minimum.
  The code already does exactly one copy (`Bytes::copy_from_slice` in `conversion.rs`).
- **Rust → PHP (deliveries):** a PHP string must live in a ZendMM allocation, so one copy is
  the floor. The code currently makes **two**: `payload.to_vec()` (Bytes→Vec) followed by
  `set_binary` (Vec→zend_string inside `Binary<u8>`'s `IntoZval`).

## Double-copy sites

1. `Delivery::payload()` — `crates/rabbit-rs-php/src/classes/delivery.rs` (`Binary::new(self.inner.payload.to_vec())`)
2. `Delivery::metadata()` binary headers — same file, `insert_header` `HeaderValue::Binary` arm
3. `Pool::get()` payload — `crates/rabbit-rs-php/src/classes/pool.rs` (`Binary::new(message.payload.to_vec())`)
   — **deferred**: this site lives in uncommitted management-API work on `main`; swap it to
   `PhpString::from_bytes` when that work lands.

## Decision

Replace the `Binary<u8>` round-trip with direct construction of a string `Zval` via
`Zval::set_zend_string(ZendStr::new(bytes, false))`, which performs a single memcpy into the
final ZendMM allocation. Both APIs already exist in ext-php-rs 0.15 (`Zval::set_zend_string`,
`IntoZval for Zval`), so **no upstream change and no PHP-facing API change** are required:
`Delivery::payload(): string` and `Pool::get(): ?array{..., payload: string}` keep their
signatures. `set_zend_string` also transparently handles interned permanent strings of length
0–1 via the `GC_IMMUTABLE` flag check.

### Changes

A bare `Zval` return type is not acceptable: `impl IntoZval for Zval` declares
`TYPE = DataType::Mixed, NULLABLE = true`, which would downgrade the generated arginfo (and
therefore the stub and `ReflectionTest`) to `mixed`. Instead, a private newtype carries the
string identity through the macro:

1. `pub(crate) struct PhpString(ZBox<ZendStr>)` in `delivery.rs` with
   `PhpString::from_bytes(&[u8])` (the single memcpy) and an `IntoZval` impl declaring
   `TYPE = DataType::String, NULLABLE = false`. The `#[php_impl]` macro reads
   `<T as IntoZval>::TYPE` into the runtime arginfo (ext-php-rs `crates/macros/src/function.rs`),
   so the generated signature stays byte-identical to today's `Binary<u8>`-based one.
2. `Delivery::payload()` returns `PhpResult<PhpString>`; `Binary` import dropped.
3. `insert_header` `HeaderValue::Binary` arm inserts `PhpString::from_bytes(value.as_ref())`.
4. `Pool::get()` uses `PhpString::from_bytes`; docblock `payload: \Ext\PhpRs\Binary` →
   `payload: string` (pool.rs); stubs regenerated via `./scripts/stubs.sh` (comment-only diff
   expected); unused `Binary` imports removed.

## Non-goals

- **True zero-copy on publish:** unsound for the reasons above; the existing single copy is
  correct and minimal. Do not propose it upstream either — the limitation is PHP's per-request
  heap model, not an ext-php-rs API gap.
- **Micro-copies** (exchange/routing_key/message_id strings, one-shot config conversion):
  negligible; YAGNI.
- **Upstream contribution:** optional ergonomics only (e.g. `IntoZval for ZBox<ZendStr>`);
  nothing essential is missing.

## Verification

Performance refactor with no observable behavior change, so this is a guarded refactor, not a
red-green TDD loop:

1. Existing suite is the regression net: Pest + PHPT (binary-safe payload round-trips) via
   `./scripts/test-extension.sh`, plus `cargo test -p rabbit-rs-php`.
2. If no existing test asserts a NUL-byte payload round-trip through `Delivery::payload()`,
   add one guard test before refactoring (must pass before and after).
3. Full quality gate at the end: `rtk ./scripts/check.sh`.
