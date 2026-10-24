---
name: Bug report
about: Report a problem with the Rust core, the native PHP extension, or the Laravel queue driver
labels: bug
---

<!-- Before filing: check docs/reference.md (troubleshooting) and the existing issues. -->

**Summary**

A concise description of the problem.

**Expected behavior**

What you expected to happen.

**Actual behavior**

What actually happened, including the exact error message. Delivery-contract bugs: state the observed losses/duplicates (`duplicates_total`, `messages_redelivered`) — silent loss after confirmed-path acceptance is a bug; duplicates are permitted but must be measurable.

**Environment**

- PHP: output of `php -v`
- Extension: output of `php --ri rabbit_rs`
- OS / arch / libc: (e.g. Ubuntu 24.04, x86_64, glibc 2.39 — or Alpine, aarch64, musl)
- SAPI: CLI / PHP-FPM / Octane (FrankenPHP, RoadRunner, Open Swoole, Swoole)
- Install method: PIE (`pie install goopil/rabbit-rs-native`), Homebrew, or source build
- Package versions: `goopil/rabbit-rs-laravel` and Laravel framework version

**Configuration**

The relevant connection/queue configuration, **redacted**: no full AMQP URIs, credentials, or certificate material. Prefer the output of `php artisan rabbit-rs:doctor`.

**Reproduction**

Minimal steps or a code snippet that reproduces the issue.
