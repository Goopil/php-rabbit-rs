# Promotion Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the storefront promotion-clean (green nightly, honest stability evidence, onboarding that matches v0.3.12), then open 5 curated-list PRs announcing Rabbit RS.

**Architecture:** Phase 0 removes the only known red CI signal (floating Runtime-app deps absorbed Octane 2.22.0 on 2026-10-14, breaking the frankenphp certification harness) by committing the Runtime app's `composer.lock`; a two-green-nightlies gate then proves stability; a storefront pass aligns README/topics/onboarding; Phase 1 opens one PR per list in each list's native format from a Goopil fork.

**Tech Stack:** bash (CI harness scripts), git/gh CLI (fork + PR mechanics), Markdown (README/lists).

**Spec:** `docs/superpowers/specs/2026-10-25-promotion-readiness-design.md`

## Global Constraints

- Nightly frankenphp root cause (verified 2026-10-25): Runtime app `composer.lock` gitignored → CI `composer update` → Octane v2.22.0 (2026-10-14) changed FrankenPHP startup (downloads ~180 MB binary) → 60 s `wait_server_up` deadline blown. The pinned FrankenPHP image is innocent (base + derived exec `/usr/local/bin/frankenphp` fine on linux/amd64).
- Tags are immutable; never force-push the Laravel mirror.
- Gate before Phase 1: two consecutive green cycles of soak (02:07 UTC), nightly/Octane (03:15), throughput (04:17), functional matrix.
- All repository artifacts in English.
- Run `rtk cargo fmt --all` only if Rust files are touched (none in this plan).

---

### Task 1: Pin the Runtime certification app dependencies (nightly fix)

**Files:**
- Modify: `packages/laravel-queue/tests/Runtime/app/.gitignore` (line 15)
- Track (un-ignore): `packages/laravel-queue/tests/Runtime/app/composer.lock` (exists locally, Octane v2.20.0 / framework v12.69.3 — the last green state)

**Interfaces:**
- Consumes: `scripts/test-octane-runtime.sh::prepare_runtime_app` — already prefers `composer install` when `composer.lock` exists (lines 201-208).
- Produces: a committed lockfile so fresh CI checkouts resolve the same dependency tree; the nightly frankenphp job returns green.

- [ ] **Step 1: Flip the ignore rule to a negation**

In `packages/laravel-queue/tests/Runtime/app/.gitignore`, replace line 15:

```gitignore
/composer.lock
```

with:

```gitignore
# The certification environment must be reproducible: the lock is committed
# and refreshed deliberately (same policy as the digest-pinned FrankenPHP
# image). Re-certifying against a newer Octane = regenerate this file.
!/composer.lock
```

The deeper `.gitignore` overrides the root `composer.lock` rule (line 6 of the root `.gitignore`).

- [ ] **Step 2: Verify the lock becomes trackable and nothing else leaks**

Run:
```bash
git check-ignore -v packages/laravel-queue/tests/Runtime/app/composer.lock; echo "exit=$?"
git status --short | head -5
```
Expected: `check-ignore` prints nothing, `exit=1` (not ignored), and `composer.lock` shows as untracked in `git status`. If other lockfiles appear as untracked, STOP — the negation over-matched; investigate before committing.

- [ ] **Step 3: Prove the CI path locally (composer install from the committed lock)**

```bash
rm -rf packages/laravel-queue/tests/Runtime/app/vendor
./scripts/test-octane-runtime.sh --server=frankenphp
```

Expected: the script runs `composer install` (not `update`) during `preparing Runtime scenario app`, then the full scenario passes: `=== starting Octane FrankenPHP server in docker`, phases 1-3 with `depth 10` assertions, exit 0. (The script boots the RabbitMQ lab itself; ~10 min.)

- [ ] **Step 4: Commit**

```bash
git add packages/laravel-queue/tests/Runtime/app/.gitignore \
        packages/laravel-queue/tests/Runtime/app/composer.lock
git commit -m "fix(ci): Commit the Runtime app composer.lock to unfloat the Octane cert

Fresh CI checkouts ran composer update against a gitignored lock, so
Laravel Octane v2.22.0 (2026-10-14) silently changed the FrankenPHP
startup under the runtime certification harness (it now downloads its
own ~180 MB binary at boot) and the 60 s wait_server_up deadline blew
up - the Octane nightly has been red since 2026-10-15. Commit the lock
at the last green state (Octane 2.20.0); re-certifying against a newer
Octane becomes a deliberate lock-regeneration diff, same policy as the
digest-pinned FrankenPHP image."
git push
```

- [ ] **Step 5: Trigger one nightly cycle now (don't wait for the schedule)**

```bash
gh workflow run nightly.yml --ref main
```

Then start the gate watch (Task 3).

---

### Task 2: Storefront pass

**Files:**
- Modify: `README.md` (new `## Stability` section before `## Limitations` at line 169)
- Repo metadata: GitHub topics + description (via `gh`, no file)

**Interfaces:**
- Consumes: archived evidence `benchmarks/results/round-k-soak/` and `benchmarks/results/round-i-rebench/`.
- Produces: README stability claims that link to archived, in-repo evidence.

- [ ] **Step 1: Add the Stability section to the README**

Insert immediately before `## Limitations`:

```markdown
## Stability

Stability is proven by scheduled kill-soaks, not by hope. The nightly soak
runs a sustained publish/consume worker against a real RabbitMQ cluster and
kills the broker repeatedly: the archived 60-minute run completed with
**0 missing messages** and **297/297 automatic reconnects**
(`benchmarks/results/round-k-soak/`), and every scenario in the chaos suite
asserts the zero-loss invariant. Results are re-verified nightly in CI
([soak workflow](https://github.com/Goopil/php-rabbit-rs/actions/workflows/soak.yml)).

Delivery stays **at-least-once** under all of it — see
[Limitations](#limitations) for what that means in practice.
```

(If the archived numbers differ from `benchmarks/results/round-k-soak/`, cite the archive's numbers — verify before committing.)

- [ ] **Step 2: Verify the archived numbers cited above**

```bash
ls benchmarks/results/round-k-soak/ && grep -rl "missing" benchmarks/results/round-k-soak/ | head -2
```

Read the archived JSON/README and confirm `missing = 0` and the reconnect count match the claim. Fix the section text if they don't.

- [ ] **Step 3: Onboarding pass against v0.3.12 reality**

Read `README.md` (Quick start) and `docs/getting-started.md` end to end, checking each of these facts; fix any drift in-place (smallest edit):
1. `pie install goopil/rabbit-rs-native` is the documented install command.
2. `composer require goopil/rabbit-rs-laravel` and PHP 8.4/8.5 support are stated.
3. The config sample uses connection-first `queue.connections.*` with `driver => 'rabbit-rs'`.
4. `php artisan vendor:publish --tag="rabbit-rs-config"` matches the package's actual tag name (`grep -rn "rabbit-rs-config" packages/laravel-queue/src/`).
5. The worker command documented is `php artisan queue:work rabbit-rs` (or the documented fan-out command) and matches the package's command registration.

- [ ] **Step 4: Set repo topics + description**

```bash
gh repo edit Goopil/php-rabbit-rs \
  --add-topic rabbitmq --add-topic php-extension --add-topic rust \
  --add-topic laravel --add-topic amqp --add-topic queue \
  --add-topic php --add-topic pie \
  --description "Native Rust RabbitMQ transport for PHP and Laravel workers — at-least-once, auto-recovering, PIE-installable."
```

- [ ] **Step 5: Commit and push**

```bash
git add README.md docs/getting-started.md
git commit -m "docs: Surface the stability evidence and pass the onboarding checklist

Adds a Stability section citing the archived zero-loss soak results,
and aligns the onboarding path with v0.3.12 (connection-first config,
PIE install, vendor:publish tag, worker command)."
git push
```

(Commit only files actually changed in Step 3.)

---

### Task 3: Nightly stability gate (two green cycles)

**Files:** none (watch + record only).

**Interfaces:**
- Consumes: Task 1's fix on main.
- Produces: the gate decision recorded in the SDD ledger — Phase 1 is blocked until this passes.

- [ ] **Step 1: Watch the Task-1-triggered run to completion**

```bash
RUN=$(gh run list --workflow=nightly.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN" --exit-status --interval 120
gh run view "$RUN" --json jobs --jq '.jobs[] | "\(.name): \(.conclusion)"'
```

Expected: all jobs green, including `Octane runtime (frankenphp)`. If it fails again, root-cause with the job log before touching anything else.

- [ ] **Step 2: Wait for the next scheduled cycle and re-verify all four workflows**

The schedules fire daily: soak 02:07, Octane nightly 03:15, throughput 04:17, functional matrix (nightly, offset from the soak). After they complete:

```bash
for wf in soak nightly throughput functional-matrix; do
  gh run list --workflow=$wf.yml --limit 1 --json conclusion,createdAt \
    --jq '.[0] | "'"$wf"': \(.conclusion) @ \(.createdAt[0:10])"'
done
```

Two consecutive days with all four green = gate passed; record both dates + run IDs in `.superpowers/sdd/<session>/progress.md`.

---

### Task 4: Packagist visibility for v0.3.12

**Files:** none (external action).

**Interfaces:**
- Consumes: mirror tag `v0.3.12` (already pushed to `Goopil/rabbit-rs-laravel`).
- Produces: Packagist serving `v0.3.12` of `goopil/rabbit-rs-laravel`.

- [ ] **Step 1: Check what Packagist currently serves**

```bash
curl -s https://repo.packagist.org/p2/goopil/rabbit-rs-laravel.json | python3 -c "import json,sys; print(json.load(sys.stdin)['packages']['goopil/rabbit-rs-laravel'][0]['version'])"
```

- [ ] **Step 2: If it still serves v0.3.11, ask the user to Update**

The mirror has no webhook and `PACKAGIST_TOKEN` is not extractable from repo secrets. Tell the user: click *Update* on https://packagist.org/packages/goopil/rabbit-rs-laravel (or paste a Packagist API token to run `./scripts/trigger-packagist-update.sh --repository https://github.com/Goopil/rabbit-rs-laravel` with `PACKAGIST_TOKEN` set). Then re-run Step 1 and expect `v0.3.12`.

---

### Task 5: The five curated-list PRs

**Files (all in upstream repos, via forks):** no rabbit-rs files change.

**Interfaces:**
- Consumes: Task 3's gate passed, Task 2's storefront pass live.
- Produces: 5 open PRs (or merged entries).

- [ ] **Step 1: Prepare the fork workspace once**

```bash
TOKEN=$(gh auth token)
ASKPASS=$(mktemp); printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$TOKEN" > "$ASKPASS"; chmod 700 "$ASKPASS"
export GIT_ASKPASS="$ASKPASS" GIT_TERMINAL_PROMPT=0
mkdir -p /tmp/promo && cd /tmp/promo
```

For each upstream below: `git clone --depth=1 https://github.com/<upstream>.git <name> && cd <name> && git checkout -b add-rabbit-rs` (default branch; if the default branch is not `master`/`main`, use `gh api repos/<upstream> --jq .default_branch`).

- [ ] **Step 2: extphprs/awesome-ext-php-rs — Networking section**

In `README.md`, section `## Networking`, after the `iggy-php` line (alphabetical: grpc-php-rs < iggy-php < rabbit-rs), add:

```markdown
- [rabbit-rs](https://github.com/Goopil/php-rabbit-rs) - Native Rust RabbitMQ (AMQP 0-9-1) transport for PHP and Laravel, with a PIE-installable extension and a Laravel queue driver.
```

Also add the section to the `Contents` list only if `Networking` is already listed (it is — no TOC change needed). Commit `Add Rabbit RS`, push to `https://github.com/Goopil/awesome-ext-php-rs.git` (`git push https://x-access-token:$TOKEN@github.com/Goopil/awesome-ext-php-rs.git add-rabbit-rs` — the fork must exist first: `gh repo fork extphprs/awesome-ext-php-rs --clone=false`), then:

```bash
gh pr create --repo extphprs/awesome-ext-php-rs --head Goopil:add-rabbit-rs \
  --title "Add Rabbit RS" \
  --body "Adds [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) to Networking: a RabbitMQ (AMQP 0-9-1) transport for PHP and Laravel built with ext-php-rs 0.16, shipped as a PIE-installable extension plus a Laravel queue driver. Alphabetically placed after iggy-php."
```

- [ ] **Step 3: ziadoz/awesome-php — Queue section**

In `README.md`, section `### Queue` (line ~603), insert after the `PHP AMQP` line (alphabetical: BunnyPHP < Pheanstalk < PHP AMQP < Rabbit RS < Tarantool Queue):

```markdown
* [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) - A native Rust RabbitMQ (AMQP 0-9-1) transport for PHP and Laravel with a PIE-installable extension.
```

Check `CONTRIBUTING.md` for issue-first rules; if the repo requires issues before PRs, open the issue instead. Commit `Add Rabbit RS to Queue`, push to the Goopil fork, `gh pr create --repo ziadoz/awesome-php --head Goopil:add-rabbit-rs --title "Add Rabbit RS to Queue" --body "One line in the Queue section: native Rust RabbitMQ transport for PHP/Laravel (AMQP 0-9-1), PIE-installable extension. Alphabetical placement between PHP AMQP and Tarantool Queue."`

- [ ] **Step 4: rabbitmq/rabbitmq-website — PHP devtools**

In `client-libraries/devtools.md`, section `## PHP {#php-dev}`, append after the `Thumper` line (do NOT use the `✓` marker — it denotes established/official clients):

```markdown
 * [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) a RabbitMQ transport for PHP and Laravel built on a native Rust runtime
```

Read the repo's `CONTRIBUTING.md` — RabbitMQ repos usually require DCO sign-off (`git commit -s`). Commit `Add Rabbit RS to the PHP devtools list` (with `-s` if required), push to fork, `gh pr create --repo rabbitmq/rabbitmq-website --head Goopil:add-rabbit-rs --title "Add Rabbit RS to the PHP client tools" --body "Adds Rabbit RS, a Rust-runtime RabbitMQ transport for PHP and Laravel, to the PHP section of the devtools page."`

- [ ] **Step 5: chiraggude/awesome-laravel — new Queues subsection**

This list has no `##### Queues` category (verified 2026-10-25). In `README.md`, after the `##### Tasks, Commands and Scheduling` block (ends before `##### Payments` at line ~222), add:

```markdown
##### Queues

* [Rabbit RS](https://github.com/Goopil/php-rabbit-rs) - Laravel queue driver backed by a native Rust RabbitMQ runtime with at-least-once delivery
```

If the top contents list references section names, add the matching entry. The PR body should present the new category as a proposal: `gh pr create --repo chiraggude/awesome-laravel --head Goopil:add-rabbit-rs --title "Add a Queues section with Rabbit RS" --body "The list has no Queues category while queue drivers are a core Laravel extension point — this proposes one, seeded with Rabbit RS (Laravel queue driver on a native Rust RabbitMQ runtime, at-least-once delivery). Easy to extend with Horizon-adjacent drivers."`

- [ ] **Step 6: johanrhodin/awesome-rabbitmq — Client libraries**

In `README.md`, section `## Client libraries` (plain `- URL - description.` format), append after the last library line:

```markdown
- https://github.com/Goopil/php-rabbit-rs - Rabbit RS, a PHP/Laravel client for AMQP 0-9-1 running on a native Rust runtime.
```

Commit `Add Rabbit RS PHP client`, push to fork, `gh pr create --repo johanrhodin/awesome-rabbitmq --head Goopil:add-rabbit-rs --title "Add Rabbit RS (PHP client)" --body "Adds Rabbit RS to Client libraries: a PHP/Laravel AMQP 0-9-1 client on a native Rust runtime."`

---

### Task 7: Root-cause the Octane phase-1 runtime regression (gate-blocking)

**Files:** TBD by bisect outcome — suspect `crates/rabbit-rs-php/` (0.16.3 migration, 63e5800) or the Runtime app's Octane version; no preemptive code changes.

**Interfaces:**
- Consumes: Task 1's committed lock (startup now green); the harness A/B method from `.superpowers/sdd/2026-10-25-promotion-readiness/task-1-report.md` (round 2).
- Produces: either an extension fix (commit on main) or a corrected lock pin (octane 2.21.0) — whatever the discriminating A/B proves. The gate (Task 3) stays blocked until the nightly's `Octane runtime (frankenphp)` job is green end to end.

Background (verified 2026-10-25): with the lock, the local harness gets past startup but fails phase 1 deterministically (2/2): `FAIL: server container rabbitrs-octane-frankenphp is still running after octane:stop`. An A/B at `bdbd49dd` (last green, Octane 2.21.0) passes all phases. Two axes changed since the last green: extension source (`bdbd49dd..HEAD`, includes the 0.16.3 migration 63e5800) AND Octane version (2.21 → 2.20 in the committed lock). Note the timeline: the 10/15 CI red was the Octane-2.22 STARTUP timeout, which masked everything until the lock fix exposed the phase-1 failure.

- [ ] **Step 1: Run the discriminating A/B — old commit, new lock's Octane version**

Worktree at `bdbd49dd`, pin the Runtime app to the committed lock's Octane (`composer require laravel/octane:2.20.0 --no-interaction` in the Runtime app or equivalent lock edit), run `./scripts/test-octane-runtime.sh --server=frankenphp` (~12 min).
- FAILS at phase 1 → the regression is in extension source: go to Step 2.
- PASSES → the regression is the Octane version: regenerate the Runtime lock at Octane **2.21.0** (the last green CI version), re-run the harness, expect green, commit the corrected lock (`fix(ci): Pin the Runtime app to Octane 2.21.0, the last green cert state`), update the report, DONE.

- [ ] **Step 2 (only if Step 1 fails): bisect the extension axis**

Bisect `bdbd49dd..HEAD` over the extension-affecting commits only (63e5800 = Cargo.toml pin + callbacks/bridge, a30b17b = zend stubs, 548474e = deny.toml; skip docs/lock-only commits) with the harness as the test. On the guilty commit: root-cause the mechanism (hypothesis to check first: the 0.16 runtime/worker threads surviving SIGTERM, so `octane:stop` never terminates the workers), fix it minimally on main (TDD: a focused test reproducing the lifecycle if feasible in Rust; otherwise harness-verified), run `rtk cargo fmt --all` + the focused checks + `./scripts/check.sh`, commit, and re-run the full harness frankenphp to green.

- [ ] **Step 3: Record the mechanism**

Append the root cause, the discriminating evidence, and the fix to the task report; the gate watch (Task 3) resumes after this lands.

---

### Task 6: Track PR outcomes

**Files:** SDD ledger only.

**Interfaces:**
- Consumes: the 5 PRs from Task 5.
- Produces: merged/closed status per PR; follow-up edits if maintainers request changes.

- [ ] **Step 1: Watch the PRs**

```bash
gh search prs --author Goopil --repo extphprs/awesome-ext-php-rs --repo ziadoz/awesome-php --repo rabbitmq/rabbitmq-website --repo chiraggude/awesome-laravel --repo johanrhodin/awesome-rabbitmq --state open --json repository,title,url
```

Check daily (or on notification). For any maintainer change request: edit the fork branch, push — the PR updates automatically. Record outcomes in the ledger.

- [ ] **Step 2: Report the final wave state**

Summarize merged/open/closed per target in the SDD ledger and to the user.
