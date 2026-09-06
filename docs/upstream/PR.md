# PR: fix(cargo-php): allow unresolved Zend symbols when linking on macOS

## Summary

`cargo install cargo-php` fails on macOS since 0.1.20 with undefined Zend engine
symbols (`_zend_empty_string`, `_zend_hash_del`, …) pulled in from `ext-php-rs`'s
`wrapper.o`. The build script already relaxes the linker for Linux
(`-Wl,--unresolved-symbols=ignore-in-object-files`) but nothing emits the macOS
equivalent, so the binary link aborts on every macOS host regardless of the local
PHP installation.

This adds the macOS branch:

```rust
#[cfg(target_os = "macos")]
println!("cargo:rustc-link-arg-bins=-Wl,-undefined,dynamic_lookup");
```

## Why this is safe

- All unresolved symbols are referenced from functions in `wrapper.c`; function
  symbols are lazily bound, and `cargo-php` never calls them.
- The stubs workflow (0.1.20+) only dlopens the extension and calls the exported
  `ext_php_rs_describe_module` metadata symbol, then renders stubs in pure Rust.
- No eagerly-bound (data) references exist outside functions, so the binary starts
  and runs with the symbols left unresolved.

## Testing

- `cargo install cargo-php` on macOS 26 (aarch64), Rust 1.96: fails with
  `Undefined symbols for architecture arm64` before the change; succeeds with the
  flag applied (`RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup"`).
- End-to-end check against an extension built with ext-php-rs 0.15.15 on Homebrew
  PHP 8.5 (NTS, **no** embed SAPI): `cargo php stubs` builds the extension,
  dlopens it, and generates correct stubs.

Fixes the issue reported as "cargo-php 0.1.21 fails to link on macOS".
