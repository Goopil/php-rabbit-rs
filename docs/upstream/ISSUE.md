# cargo-php 0.1.21 fails to link on macOS: undefined Zend symbols (Linux-only link flag in build.rs)

## Summary

`cargo-php` v0.1.21 does not compile on macOS. The link step of the `cargo-php` binary fails with undefined Zend engine symbols coming from `ext-php-rs`'s `wrapper.o`.

The package's build script relaxes symbol resolution for **Linux only** (`-Wl,--unresolved-symbols=ignore-in-object-files`) but has no equivalent for **macOS** (`-Wl,-undefined,dynamic_lookup`), so every macOS user hits the same hard link failure — independently of their PHP installation.

## Environment

- cargo-php 0.1.21 (installed via `cargo install cargo-php`)
- Host: macOS 26.x, aarch64-apple-darwin
- Extension under test built with ext-php-rs 0.15.15
- Reproducible regardless of the local PHP build (no `-lphp` is linked without the `embed` feature)

## Reproduction

```console
$ cargo install cargo-php
   ...
error: linking with `cc` failed: exit status: 1
  |
  = note: Undefined symbols for architecture arm64:
            "_zend_empty_string", referenced from:
                _ext_php_rs_zend_string_init in libext_php_rs-…rlib[…](…-wrapper.o)
            "_zend_hash_del", referenced from:
                ext_php_rs::types::array::<impl ext_php_rs::ffi::_zend_array>::remove::…
            "_zend_hash_find", referenced from:
                …
          clang: error: linker command failed with exit code 1 (use -v to show invocation)
error: could not compile `cargo-php` (bin "cargo-php") due to 1 previous error
```

## Root cause

`cargo-php`'s build script already documents the situation:

```rust
// ext-php-rs wrapper.c includes functions that call Zend engine symbols
// only available inside a running PHP process. cargo-php never calls
// these functions, but the linker still sees the references. Allow them
// to remain unresolved.
#[cfg(target_os = "linux")]
println!("cargo:rustc-link-arg-bins=-Wl,--unresolved-symbols=ignore-in-object-files");
```

The Linux branch exists, but executables on macOS are linked with the default `-undefined error`, and the same unresolved references from `wrapper.o` (and the static `ext-php-rs` rlib objects) abort the link. Unlike shared libraries — where macOS allows undefined symbols by default — a **binary** target must be told explicitly that the Zend symbols may stay unresolved.

## Why `-undefined dynamic_lookup` is safe here

All the reported undefined symbols are referenced from **functions** in `wrapper.c` (and code paths behind them). Function symbols are lazily bound, and `cargo-php` never calls the wrapper functions: since 0.1.20 the stubs workflow dlopens the extension and calls only the exported `ext_php_rs_describe_module` metadata symbol, then renders the stub text in pure Rust. No eagerly-bound (data) references exist outside functions, so the binary starts and runs fine with the symbols left unresolved.

## Workaround (until fixed)

```console
$ RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo install cargo-php
```

Verified working end to end on macOS with Homebrew PHP 8.5 (which has **no** embed SAPI at all): `cargo php stubs` builds the extension, dlopens it, and renders correct stubs.

## Proposed fix

```diff
--- a/crates/cli/build.rs
+++ b/crates/cli/build.rs
@@ -11,6 +11,9 @@ fn main() {
     // these functions, but the linker still sees the references. Allow them
     // to remain unresolved.
     #[cfg(target_os = "linux")]
     println!("cargo:rustc-link-arg-bins=-Wl,--unresolved-symbols=ignore-in-object-files");
+
+    #[cfg(target_os = "macos")]
+    println!("cargo:rustc-link-arg-bins=-Wl,-undefined,dynamic_lookup");
 }
```

Happy to send this as a PR if you agree with the approach.
