# Rabbit RS PHP Extension API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Load `ext-rabbit_rs` into PHP 8.4 and expose the validated public `Goopil\RabbitRs` contract for the rest of Milestone B.

**Architecture:** `ext-php-rs` remains a thin boundary above `rabbit-rs-core`. This first delivery registers the module, its version, the exceptions, and the final classes, but explicitly refuses the operations whose conversions and handles will be implemented in Tasks 14 and 15. The PHPT tests load the real `cdylib` artifact and verify the contract through reflection.

**Tech Stack:** Rust 1.96, edition 2024, ext-php-rs 0.15.15, PHP 8.4, PHPT `run-tests.php`, Cargo, Composer/PIE.

## Global Constraints

- The PIE package is named `goopil/rabbit-rs-native`.
- The native namespace is exactly `Goopil\RabbitRs`.
- The technical name loaded by PHP remains `rabbit_rs`.
- The minimum PHP version remains 8.4 and `php`/`php-config` must share the same major and minor version.
- `#![forbid(unsafe_code)]` remains active; no manual unsafe code is allowed.
- No Lapin type crosses the PHP boundary.
- No PHP object, `Zval`, callback, request, or service container is retained in a Rust thread.
- An unwired operation throws `Goopil\RabbitRs\Exception`; it never returns a fake success.
- Future payloads remain binary; the native signature of `Delivery::payload()` uses `Binary<u8>`.

---

### Task 1: Load the module and register the stable exceptions

**Files:**
- Create: `.cargo/config.toml`
- Modify: `composer.json`
- Modify: `crates/rabbit-rs-php/Cargo.toml`
- Modify: `crates/rabbit-rs-php/src/lib.rs`
- Create: `crates/rabbit-rs-php/src/classes/mod.rs`
- Create: `crates/rabbit-rs-php/src/classes/exception.rs`
- Create: `crates/rabbit-rs-php/tests/phpt/extension_metadata.phpt`
- Create: `scripts/test-extension.sh`

**Interfaces:**
- Consumes: `ext_php_rs::prelude::{ModuleBuilder, PhpResult}`, `ext_php_rs::zend::ce::exception`, and `env!("CARGO_PKG_VERSION")`.
- Produces: module `rabbit_rs`, `Goopil\RabbitRs\Exception`, `BackpressureException`, `ConnectionException`, and `unavailable<T>() -> PhpResult<T>` for Task 2.

- [ ] **Step 1: Add the PHPT runner and failing module test**

Create `scripts/test-extension.sh` as an executable Bash script. It must:

1. resolve `php` and `php-config` from `PHP_BIN`/`PHP_CONFIG` overrides or `PATH`;
2. reject a version mismatch between the two tools;
3. locate `run-tests.php` below `$(php-config --prefix)/lib/php/build/`;
4. locate `target/release/librabbit_rs_php.so` on Linux or `target/release/librabbit_rs_php.dylib` on macOS;
5. derive the `rabbit-rs-php` version from `cargo metadata --no-deps --format-version=1` and export it as `RABBIT_RS_EXPECTED_VERSION`;
6. pass `-n -d extension=<absolute artifact>` to `run-tests.php`;
7. select `*<filter>*.phpt` when the optional first argument is present;
8. set `NO_INTERACTION=1` and `REPORT_EXIT_STATUS=1` and propagate the PHPT exit status.

Create `extension_metadata.phpt` with this observable contract:

```php
--TEST--
Rabbit RS extension metadata and exception hierarchy
--FILE--
<?php
function expect_true(bool $condition, string $message): void {
    if (!$condition) {
        throw new Exception($message);
    }
}

expect_true(extension_loaded('rabbit_rs'), 'rabbit_rs is not loaded');
expect_true(
    phpversion('rabbit_rs') === getenv('RABBIT_RS_EXPECTED_VERSION'),
    'extension version does not match Cargo'
);
expect_true(is_subclass_of(Goopil\RabbitRs\Exception::class, Exception::class), 'base exception');
expect_true(is_subclass_of(Goopil\RabbitRs\Exception::class, Throwable::class), 'base throwable');
expect_true(is_subclass_of(Goopil\RabbitRs\BackpressureException::class, Goopil\RabbitRs\Exception::class), 'backpressure exception');
expect_true(is_subclass_of(Goopil\RabbitRs\ConnectionException::class, Goopil\RabbitRs\Exception::class), 'connection exception');
expect_true((new ReflectionClass(Goopil\RabbitRs\BackpressureException::class))->isFinal(), 'backpressure final');
expect_true((new ReflectionClass(Goopil\RabbitRs\ConnectionException::class))->isFinal(), 'connection final');
echo "OK\n";
?>
--EXPECT--
OK
```

- [ ] **Step 2: Run the RED test**

Run:

```bash
rtk cargo build -p rabbit-rs-php --release
rtk ./scripts/test-extension.sh extension_metadata
```

Expected: FAIL because the current `cdylib` has no PHP `get_module` entry point and no registered classes.

- [ ] **Step 3: Register ext-php-rs and the exception hierarchy**

Create the root `.cargo/config.toml` required by ext-php-rs for dynamically
loaded PHP extensions on Linux and macOS. PHP resolves the intentionally
undefined Zend symbols when it loads the extension:

```toml
[target.'cfg(not(target_os = "windows"))']
rustflags = ["-C", "link-arg=-Wl,-undefined,dynamic_lookup"]
```

Pin the dependency:

```toml
[dependencies]
ext-php-rs = "=0.15.15"
rabbit-rs-core = { path = "../rabbit-rs-core" }
```

Change the root Composer name to `goopil/rabbit-rs-native` without changing `extension-name`.

In `exception.rs`, use ext-php-rs's safe `ext_php_rs::zend::ce::exception`
accessor during module startup. The Rabbit RS base exception extends
`\Exception` and therefore implements `\Throwable` transitively. Do not
declare `implements Throwable` directly because PHP forbids that on user
classes. Register:

```rust
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Exception")]
#[php(extends(ce = ce::exception, stub = "\\Exception"))]
#[derive(Default)]
pub struct RabbitRsException;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\BackpressureException")]
#[php(extends(RabbitRsException))]
#[php(flags = ClassFlags::Final)]
#[derive(Default)]
pub struct BackpressureException;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\ConnectionException")]
#[php(extends(RabbitRsException))]
#[php(flags = ClassFlags::Final)]
#[derive(Default)]
pub struct ConnectionException;
```

Also expose this crate-private helper:

```rust
pub(crate) fn unavailable<T>(operation: &str) -> PhpResult<T> {
    Err(PhpException::from_class::<RabbitRsException>(format!(
        "{operation} is not available before native handle initialization"
    )))
}
```

In `lib.rs`, keep `#![forbid(unsafe_code)]`, declare `mod classes`, and register the module explicitly:

```rust
#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .name("rabbit_rs")
        .version(env!("CARGO_PKG_VERSION"))
        .class::<RabbitRsException>()
        .class::<BackpressureException>()
        .class::<ConnectionException>()
}
```

- [ ] **Step 4: Run the GREEN test and metadata checks**

Run:

```bash
rtk cargo fmt --all
rtk cargo build -p rabbit-rs-php --release
rtk ./scripts/test-extension.sh extension_metadata
rtk composer validate --strict
```

Expected: PASS with exactly `OK` from the PHPT test and Composer package name `goopil/rabbit-rs-native`.

- [ ] **Step 5: Commit**

```bash
rtk git add .cargo/config.toml Cargo.lock composer.json crates/rabbit-rs-php scripts/test-extension.sh
rtk git commit -m "feat(extension): register Goopil namespace and exceptions"
```

---

### Task 2: Expose the final classes and their signatures

**Files:**
- Create: `crates/rabbit-rs-php/src/classes/pool.rs`
- Create: `crates/rabbit-rs-php/src/classes/consumer.rs`
- Create: `crates/rabbit-rs-php/src/classes/delivery.rs`
- Modify: `crates/rabbit-rs-php/src/classes/mod.rs`
- Modify: `crates/rabbit-rs-php/src/lib.rs`
- Create: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Create: `crates/rabbit-rs-php/tests/phpt/reflection.phpt`

**Interfaces:**
- Consumes: `classes::exception::unavailable<T>()` from Task 1.
- Produces: final PHP classes `Pool`, `Consumer`, `Delivery` with the signatures approved in the product plan.

- [ ] **Step 1: Write the failing reflection test**

Create `reflection.phpt`. It must inspect real `ReflectionClass` and `ReflectionMethod` objects and fail unless:

- `Goopil\RabbitRs\Pool`, `Consumer`, and `Delivery` exist and are final;
- `Pool::__construct(array $config)` exists;
- `Pool::publish(array $message): string`, `publishBatch(array $messages): array`, `consumer(string $profile): Consumer`, `stats(): array`, and `close(): void` exist;
- `Consumer::next(int $timeoutMs): ?Delivery` and `close(): void` exist;
- `Delivery::payload(): string`, `metadata(): array`, `ack(): void`, `release(int $delayMs = 0): void`, and `reject(bool $requeue = false): void` exist;
- constructing `Pool` raises `Goopil\RabbitRs\Exception` with a non-empty, secret-free message.

Use a local PHP helper that compares literal reflection type strings, optionality, and defaults, then print only `OK` under `--EXPECT--`.

- [ ] **Step 2: Run the RED reflection test**

Run:

```bash
rtk cargo build -p rabbit-rs-php --release
rtk ./scripts/test-extension.sh reflection
```

Expected: FAIL because the three operational classes are not registered.

- [ ] **Step 3: Implement the minimal final classes**

Use `#[php_class]`, fully qualified `#[php(name = "Goopil\\RabbitRs\\...")]`, and `#[php(flags = ClassFlags::Final)]` on all three structs. They are intentionally zero-sized in Task 13 because no instance may be successfully created before Task 14 establishes validated native handles.

Use these Rust boundary types so reflection and future binary safety are correct:

```rust
// Pool
pub fn __construct(config: &ZendHashTable) -> PhpResult<Self>;
pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String>;
pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>>;
pub fn consumer(&self, profile: String) -> PhpResult<Consumer>;
pub fn stats(&self) -> PhpResult<ZBox<ZendHashTable>>;
pub fn close(&self) -> PhpResult<()>;

// Consumer
pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>>;
pub fn close(&self) -> PhpResult<()>;

// Delivery
pub fn payload(&self) -> PhpResult<Binary<u8>>;
pub fn metadata(&self) -> PhpResult<ZBox<ZendHashTable>>;
pub fn ack(&self) -> PhpResult<()>;
#[php(defaults(delayMs = 0))]
pub fn release(&self, delayMs: i64) -> PhpResult<()>;
#[php(defaults(requeue = false))]
pub fn reject(&self, requeue: bool) -> PhpResult<()>;
```

Every method calls `unavailable()` with its fully qualified operation name. Because `ext-php-rs` 0.15.15 preserves Rust parameter identifiers in PHP named arguments, keep the contractual identifiers, including camelCase names such as `timeoutMs` and `delayMs`, and consume unused values explicitly in the method body. Do not retain any `ZendHashTable` reference.

Register exception classes before operational classes in `module()`.

- [ ] **Step 4: Add the authoritative PHP stub**

Create `rabbit_rs.stub.php` with `declare(strict_types=1)`, namespace `Goopil\RabbitRs`, the exception hierarchy, and the exact final class signatures from the product plan. Method bodies remain empty because this is a static-analysis/reflection contract, not runtime PHP fallback code.

- [ ] **Step 5: Run focused GREEN verification**

Run:

```bash
rtk cargo fmt --all
rtk cargo build -p rabbit-rs-php --release
rtk ./scripts/test-extension.sh
rtk php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
```

Expected: both PHPT files PASS and the stub reports no syntax errors.

- [ ] **Step 6: Run the repository gate**

Run:

```bash
rtk ./scripts/check.sh
```

Expected: PASS for formatting, Clippy with all features/targets, all Rust tests, and strict Composer validation.

- [ ] **Step 7: Commit and update progress**

Update `docs/plans/2026-07-30-rabbitmq-native-implementation.md` so Task 13 is checked, the next step is Task 14, and the checkpoint records the verified test counts.

```bash
rtk git add crates/rabbit-rs-php scripts docs/plans/2026-07-30-rabbitmq-native-implementation.md
rtk git commit -m "feat(extension): expose native pool publisher and consumer API"
```

## Plan Self-Review

- Naming spec coverage: package, namespace, extension name, PHP floor, version and exceptions are mapped to Tasks 1–2.
- Task 13 product coverage: every planned class and method has a reflection assertion and stub declaration.
- Safety coverage: no successful placeholder operation, no retained Zend values, no exposed Lapin type, and `unsafe_code` remains forbidden.
- Exception consistency: `Goopil\RabbitRs\Exception` extends `\Exception`, is transitively `\Throwable`, and never implements `Throwable` directly.
- Scope boundary: Tasks 14 and 15 are deliberately excluded; they will replace `unavailable()` with conversion/state and lifecycle behavior through separate TDD plans.
- Type consistency: `Binary<u8>` maps payloads to PHP strings, `ZendHashTable` maps PHP arrays without premature owned conversion, and `i64` maps PHP integers.
- Placeholder scan: no `TBD`, `TODO`, or unspecified implementation step remains.
