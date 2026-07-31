# Goopil Rabbit RS Naming Design

**Date:** 2026-07-31

**Status:** Approved

## Decision

Rabbit RS uses `Goopil` as its public PHP and Composer vendor identity.

- Native extension package: `goopil/rabbit-rs-native`
- Laravel package: `goopil/rabbit-rs-laravel`
- Native PHP namespace: `Goopil\RabbitRs`
- Laravel PHP namespace: `Goopil\RabbitRs\Laravel`
- PHP extension name: `rabbit_rs` (unchanged)
- Rust workspace and crate names: unchanged

The extension classes are therefore `Goopil\RabbitRs\Pool`,
`Goopil\RabbitRs\Consumer`, and `Goopil\RabbitRs\Delivery`. The stable
exception hierarchy starts at `Goopil\RabbitRs\Exception`, with
`BackpressureException` and `ConnectionException` as specialized children.

## Milestone B Boundary

The naming decision is recorded on `main` before implementation. Milestone B
implementation then proceeds on the dedicated branch
`feature/rabbit-rs-php-extension`.

Task 13 exposes the approved final classes, method signatures, extension
metadata, exception hierarchy, stubs, and PHPT reflection contract. Operations
whose native behavior belongs to later tasks fail explicitly with a stable
`Goopil\RabbitRs\Exception`; they never return placeholder success values.

PHP objects retain only native handles or identifiers. No Lapin type, Zend
value, PHP callback, request, or service-container state crosses into Rust
actor threads.

## Compatibility and Distribution

The public minimum is PHP 8.4. Local compilation and PHPT execution use the
same PHP 8.4 installation through matching `php` and `php-config` binaries.
Package metadata, release documentation, stubs, tests, and future Laravel code
must use the approved Goopil names consistently.

The extension version comes from the Cargo package version and must match
`phpversion('rabbit_rs')` and release tags. The extension remains installable
through PIE; changing the Composer vendor does not change the technical
extension name loaded by PHP.

## Verification

Reflection tests load the compiled extension into PHP 8.4 and verify class
names, final modifiers, method signatures, exception inheritance, extension
name, and version. Later PHPT tasks add value conversion, state transition,
fork, CLI, and FPM behavior without changing this public naming contract.
