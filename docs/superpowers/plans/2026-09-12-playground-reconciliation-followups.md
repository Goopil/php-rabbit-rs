# Playground Reconciliation Follow-ups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the three small items of the playground reconciliation (#253 items 1, 2, 4): pin the unroutable-return counter (#252), document the #211 keep-alive mitigation (#254), and record the already-fixed doctor lint.

**Architecture:** No production behavior change. One integration test assertion pins the existing countable-outcome contract; one docs bullet documents the existing keep-alive mechanism; the doctor-lint item needs only issue comments. If the new assertion fails, that is a real #252 gap — stop and file the finding before coding.

**Tech Stack:** PHP 8.4 Pest integration tests (ext-rabbit_rs from `target/`, never system-wide), Markdown docs, `gh` CLI for issue hygiene, RabbitMQ lab (docker, already running).

**Spec:** `docs/superpowers/specs/2026-09-12-playground-reconciliation-followups-design.md`

## Global Constraints

- Repository artifacts (docs, commits, issue comments) in English.
- The extension is loaded from `target/debug/` (or `target/release/`) via the test scripts — never installed system-wide.
- No production Rust or PHP source changes in this plan. The only repo changes are one test file and one docs file.
- TDD where behavior changes: here the assertion pins existing behavior, so expected result is PASS; a FAIL is a finding, not something to force green.
- Preserve unrelated working-tree changes; never `git add -A` (the `scratch/` directory is untracked and must stay so).
- Verify each step's command and report the actual result before moving on.

---

### Task 1: Branch + #252 counter pin in `PublishErrorSurfacingTest`

**Files:**
- Modify: `packages/laravel-queue/tests/Integration/PublishErrorSurfacingTest.php:62-64` (first test case, after the existing `expect`)

**Interfaces:**
- Consumes: `$this->pool` (native `Goopil\RabbitRs\Pool`, set by the `integrationPoolAndQueue()` helper in `tests/Pest.php`); `Pool::stats()['returns_total']` (documented in `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`).
- Produces: an integration assertion that an unroutable mandatory publish increments `returns_total` — later relied on by the #252 close comment.

- [ ] **Step 1: Create the branch**

```bash
rtk git checkout -b fix/playground-reconciliation-followups
```

- [ ] **Step 2: Extend the first test case with the counter assertion**

In `packages/laravel-queue/tests/Integration/PublishErrorSurfacingTest.php`, the first test currently ends at:

```php
    expect($thrown)->not->toBeNull('an unroutable mandatory publication must surface at the next pop')
        ->and($thrown->getMessage())->toContain('unroutable');
});
```

Replace with:

```php
    expect($thrown)->not->toBeNull('an unroutable mandatory publication must surface at the next pop')
        ->and($thrown->getMessage())->toContain('unroutable');

    // The outcome must also be countable without a follow-up publish
    // operation (issue #252): the publisher actor records the broker
    // return in the metrics snapshot read by stats().
    expect($this->pool->stats()['returns_total'])->toBeGreaterThan(0);
});
```

- [ ] **Step 3: Run the focused integration test against the lab**

```bash
rtk ./scripts/test-laravel.sh tests/Integration/PublishErrorSurfacingTest.php
```

Expected: PASS (both cases; the script auto-builds the extension and loads it from `target/`). The lab is already running (`docker ps` shows `rabbitrs-rabbitmq-1..3`).

**Failure path:** if the new assertion fails (`returns_total` not incremented), this is the real #252 gap. STOP: do not weaken the assertion. Record the output, file the finding on #252, and implement the minimal fix only after user approval.

- [ ] **Step 4: Commit**

```bash
rtk git add packages/laravel-queue/tests/Integration/PublishErrorSurfacingTest.php
rtk git commit -m "test(laravel): pin the returned-publication counter for unroutable safe publishes (#252)"
```

---

### Task 2: #254 docs bullet — keep-alive mitigation

**Files:**
- Modify: `packages/laravel-queue/docs/reference.md` (section "Release semantics (TTL mode)", bullet list "What this means for timing", insert after the "The bucket is a floor, not a ceiling" bullet, currently lines 1093-1098)

**Interfaces:**
- Consumes: the existing ceiling bullet (unbounded past-TTL wait, broker lazy expiry).
- Produces: docs parity with the implemented mitigation — `DelayKeepAlive` in `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:448` periodically re-declares live bucket destinations; the keep-alive period derives from the expiry margin (`crates/rabbit-rs-core/src/topology/delay.rs:138`).

- [ ] **Step 1: Insert the new bullet**

In `packages/laravel-queue/docs/reference.md`, directly after the bullet ending with "routes each message through the `x-delayed-message` exchange at the exact requested delay." (line 1098), insert:

```markdown
- **In-flight delayed jobs are protected from bucket-queue deletion.** The
  connection periodically re-declares its live bucket queues (`DelayKeepAlive`,
  issue #211), so the `x-expires` idleness window cannot delete a queue that
  still holds messages. The unbounded wait above stays broker lazy-TTL
  semantics — a late release, never a silent loss.
```

- [ ] **Step 2: Run the docs coherence gate**

```bash
rtk ./scripts/check-docs.sh
```

Expected: green (no stale-claim phrase triggered; the new text introduces no prohibited claim).

- [ ] **Step 3: Commit**

```bash
rtk git add packages/laravel-queue/docs/reference.md
rtk git commit -m "docs(laravel): note the keep-alive mitigation for quorum-TTL lazy release (#254)"
```

---

### Task 3: Full gates + issue hygiene

**Files:** none (commands and issue comments only).

- [ ] **Step 1: Full quality gate**

```bash
rtk ./scripts/check.sh
```

Expected: green (fmt, clippy, nextest, composer validate — Rust untouched, but the gate is the completion contract).

- [ ] **Step 2: Full Laravel suite (Unit + Feature, no extension)**

```bash
rtk ./scripts/test-laravel.sh
```

Expected: green (was 464 passed before the change).

- [ ] **Step 3: Full integration suite**

```bash
rtk ./scripts/test-integration.sh
```

Expected: green with the lab running.

- [ ] **Step 4: Push and open the PR**

```bash
rtk git push -u origin fix/playground-reconciliation-followups
```

```bash
gh pr create --title "test(laravel): pin returned-publication counter; docs: #211 keep-alive mitigation" --body "$(cat <<'EOF'
## Summary

Playground reconciliation follow-ups (#253 items 1, 2, 4) — no production code change:

- **#252**: extend `PublishErrorSurfacingTest` to pin that an unroutable mandatory (safe-mode) publish is countable via `Pool::stats()['returns_total'] >= 1` without a follow-up publish operation. The fail-loud half was already pinned by the same test since Round D.
- **#254**: document in the delay reference that live bucket queues are kept alive by periodic re-declaration (`DelayKeepAlive`, #211), so `x-expires` cannot delete a queue holding in-flight delayed jobs; the lazy-TTL ceiling remains a late release, never a loss.
- **#253 item 4** (doctor lint contradiction): already resolved by #190 — `RabbitMqDoctorCommandTest` asserts no inheritance-trap warning; no repo change.

## Test plan

- `./scripts/test-laravel.sh tests/Integration/PublishErrorSurfacingTest.php` (lab)
- `./scripts/check-docs.sh`
- `./scripts/check.sh`, `./scripts/test-laravel.sh`, `./scripts/test-integration.sh`

Closes #252, closes #254.
EOF
)"
```

- [ ] **Step 5: Comment on #253**

```bash
gh issue comment 253 --body "$(cat <<'EOF'
Reconciliation items 1, 2 and 4 resolved on `fix/playground-reconciliation-followups`:

- **Item 1 (#254)**: the keep-alive re-declaration of live bucket queues (#211) is now documented next to the quorum-TTL ceiling in the delay reference.
- **Item 2 (#252)**: the fail-loud half was already pinned (`PublishErrorSurfacingTest`, Round D); the countable half is now pinned too (`stats()['returns_total'] >= 1` after an unroutable publish).
- **Item 4 (doctor lint)**: already resolved by #190 — the doctor reports the inherited worker without warning, pinned by `RabbitMqDoctorCommandTest`.

Items 3 (doctor DLX canary, #219) and 5/6 (#255 measurement, `--stop-when-empty`) stay tracked.
EOF
)"
```

Note: #252 and #254 are closed by the PR (`Closes` lines). #253 stays open (items 3, 5, 6 remain).
