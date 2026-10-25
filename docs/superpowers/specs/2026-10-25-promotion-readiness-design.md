# Promotion readiness: clean storefront, then awesome-list PRs

Date: 2026-10-25
Status: approved design (brainstorm 2026-10-25)
Follows: v0.3.12 release + ext-php-rs 0.16.3 migration (#359, PRs #364/#365)

## Goal

Announce Rabbit RS on curated lists (5 PRs) once the storefront a visitor
lands on is clean: green CI phases, honest stability evidence, and an
onboarding path that matches the shipped product. No launch posts — list
PRs only (user decision, 2026-10-25).

## Phase 0 — storefront clean (blocking, before any PR)

### 0.1 Fix the red nightly (root-caused 2026-10-25)

`Octane runtime (frankenphp)` has failed 11 nights in a row (2026-10-15 →
2026-10-25). Root cause: the Runtime certification app's `composer.lock` is
gitignored, so fresh CI checkouts run `composer update` and float
dependencies. Laravel Octane v2.22.0 (released 2026-10-14) changed the
FrankenPHP startup — it warns "binary version may be incompatible" and
downloads its own ~180 MB static binary at boot — blowing the harness's 60 s
`wait_server_up` deadline. The pinned FrankenPHP image is innocent: base and
derived (+pcntl) images execute `/usr/local/bin/frankenphp` fine on
linux/amd64 (verified locally 2026-10-25).

Fix: commit the Runtime app's `composer.lock` (currently at Octane 2.20.0 /
framework v12.69.3 — the last green state), remove it from
`packages/laravel-queue/tests/Runtime/app/.gitignore`. The harness already
prefers `composer install` when a lock exists
(`scripts/test-octane-runtime.sh::prepare_runtime_app`). Re-certifying
against a newer Octane becomes a deliberate, reviewable lock-regeneration
diff — same philosophy as the digest-pinned FrankenPHP image and the pinned
PIE/rust toolchains.

Verification: run the frankenphp harness locally against the lab; then the
gate below.

### 0.2 Stability gate (user requirement)

Two consecutive green nightly cycles of all four scheduled workflows — soak
(02:07 UTC), Octane nightly (03:15), throughput (04:17), functional matrix
— before Phase 1 starts.

### 0.3 Storefront pass

- GitHub topics + description (rabbitmq, php-extension, rust, laravel,
  amqp, pie, queue).
- README: a short stability evidence pointer (soak numbers + archived
  results path) so the "designed to stay alive" claim is clickable.
- Onboarding pass on `docs/getting-started.md` and the README quickstart
  against v0.3.12 reality (connection-first config, extension constraint,
  vendor:publish).
- Packagist: `goopil/rabbit-rs-laravel` still serves v0.3.11 (mirror has no
  webhook) — trigger the Update (click or `scripts/trigger-packagist-update.sh`
  with a Packagist token) and verify v0.3.12 resolves.

## Phase 1 — the five list PRs (after the gate)

One entry per list, in each list's native format, neutral-technical tone;
the 4-6× consumer benchmark number only where the list tone allows it.
PRs from forks under the Goopil account, one commit each.

| Repo | Section | Entry angle |
|---|---|---|
| extphprs/awesome-ext-php-rs | Networking (after iggy-php, alphabetical) | RabbitMQ (AMQP 0-9-1) transport for PHP/Laravel built on ext-php-rs; PIE-installable |
| ziadoz/awesome-php | Queues | Native Rust RabbitMQ transport for PHP/Laravel, 4-6× consumer throughput vs php-amqplib/ext-amqp |
| rabbitmq/rabbitmq-website | DevTools / PHP clients | Rabbit RS — native Rust PHP transport (link per site conventions) |
| chiraggude/awesome-laravel | Queues | Queue connection driver backed by the native Rust runtime |
| johanrhodin/awesome-rabbitmq | Client libraries | PHP/Laravel client built on a Rust runtime |

Rejected: awesome-amqp (AMQP 1.0-only; we speak 0-9-1), creating an
awesome-pie list (premature, YAGNI).

## Out of scope

Show HN / Reddit / newsletter posts (user deferred), Octane 2.22
re-certification (follow-up round if wanted), new docs pages.
