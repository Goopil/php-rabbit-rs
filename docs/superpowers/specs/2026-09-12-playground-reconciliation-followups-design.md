# Playground reconciliation follow-ups — design

Date: 2026-09-12
Status: Approved (mini-lot C of the v1 consolidation — see issue #253, items 1, 2 and 4)

## Context

The external playground reconciliation (#253) left three small consolidation
items. Exploration on current main (v0.3.1+) showed two of them are partially
or fully satisfied already; this design scopes the verified residual delta
only. No new features, no production behavior change expected.

## Item 1 — #254: document the #211 keep-alive mitigation (docs only)

`packages/laravel-queue/docs/reference.md` already documents the quorum-TTL
lazy-release ceiling in the "Release semantics (TTL mode)" section
("The bucket is a floor, not a ceiling", unbounded past-TTL wait on an idle
broker). The residual delta is the mitigation: #211 (closed) introduced the
keep-alive re-declaration of live bucket queues so `x-expires` can no longer
delete a bucket queue holding in-flight delayed jobs.

**Change:** one bullet in the "What this means for timing" list, stating that
live bucket queues are kept alive by re-declaration (#211) — the residual
ceiling above remains broker semantics (lazy TTL expiry), not a deletion risk.

## Item 2 — #252: pin the unroutable counter (test only, no production code)

Safe-mode unroutable observability has two halves:

- **Fail loud** — an unroutable mandatory publish surfaces as an exception at
  the next operation. Already pinned: `PublishErrorSurfacingTest`
  (`tests/Integration/PublishErrorSurfacingTest.php:43`), shipped in Round D.
- **Countable** — `Pool::stats()['returns_total']` (exposed since #156, shown
  by `rabbit-rs:status`) must be ≥ 1 after an unroutable publish, without
  requiring a follow-up publish operation. NOT pinned by any test today.

**Change:** extend the existing single-publish case of
`PublishErrorSurfacingTest` (`tests/Integration/PublishErrorSurfacingTest.php:43`)
with one assertion: after the unroutable publish surfaces, read the native
pool `stats()` and assert `returns_total >= 1`. Runs against the lab via
`./scripts/test-integration.sh`.

If the probe reveals an actual gap (counter not incremented), file the
finding on #252 and implement the minimal fix before closing — verify first,
code only what is missing.

## Item 3 — doctor lint contradiction (#253 item 4): already resolved

`RabbitMqDoctorCommandTest.php:72` asserts the doctor reports the inherited
worker **without warning** (no "inheritance trap"), fixed by #190. The full
Laravel suite is green (464 tests). No repo change; close the item on #253
with the test reference as evidence.

## Issue hygiene

- Close #254 with the docs diff as evidence.
- Close #252 with the new test as evidence (or the follow-up if a gap shows).
- Comment on #253 marking items 1, 2 and 4 reconciled.

## Verification

- `rtk ./scripts/test-laravel.sh` (Unit + Feature, no extension).
- `rtk ./scripts/test-integration.sh` (new stats assertion; lab is running).
- `rtk ./scripts/check-docs.sh` (docs coherence gate).
- `rtk composer validate --strict`.

## Non-goals

- Blind-mode wire-level return counting (documented fire-and-forget contract,
  #253 non-goal).
- Doctor unroutable canary (candidate only if the probe exposes a gap; the
  DLX canary #219 stays a separate item).
- #255 flush-timer measurement and #219 DLX canary — later items of block C.
