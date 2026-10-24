<!-- One logical change per PR. Keep the delivery contract in mind: at-least-once, silent loss is unacceptable, duplicates must remain identifiable and measurable. -->

## What & why

<!-- What does this PR change, and why? Link the issue/audit finding if one exists. -->

## Tests

<!-- Which verification did you run? Paste the key results. -->

- [ ] Focused tests for the changed behavior (added/updated first)
- [ ] `rtk ./scripts/check.sh` (Rust quality gate) — for Rust changes
- [ ] `./scripts/test-extension.sh` — for PHP extension changes
- [ ] `./scripts/test-laravel.sh` — for Laravel driver changes
- [ ] `./scripts/test-integration.sh` — when broker behavior is involved

## Checklist

- [ ] No unsafe Rust; `#![forbid(unsafe_code)]` untouched
- [ ] No credentials, full broker URIs, or certificate material in logs, errors, metrics, or test fixtures
- [ ] Bounded queues, channels, in-flight work, retries, and replay buffers
- [ ] Docs/stubs updated (English), CHANGELOG entry for user-visible changes
