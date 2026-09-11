# Reference

Reference documentation for the native extension: installation and distribution, the reliability contract, and troubleshooting.

**Contents**

- [Installation](#installation) — prerequisites, PIE, Homebrew, Docker, multi-PHP, upgrades
- [Reliability](#reliability) — the at-least-once contract, safety modes, recovery, duplicates, metrics
- [Troubleshooting](#troubleshooting) — common errors and diagnosis

## Installation

Installing the Rabbit RS native extension and the Laravel queue driver.

### Prerequisites

- PHP 8.4 or 8.5 (**NTS only** — ZTS is not supported in V1, see [Thread safety](#thread-safety))
- Linux x86_64 or ARM64 (glibc or musl)
- RabbitMQ 4.2.9 or newer (reachable from your PHP process — the CI lab runs 4.2.9)
- [PIE](https://github.com/php/pie) 1.4.10+ for extension installation (the version the release pipeline validates against)
- [Composer](https://getcomposer.org) for the Laravel queue driver

> **macOS** (Apple Silicon) is supported through the Homebrew tap or a manual release download; Windows is not supported in V1. macOS installs are validated best-effort — see [How pre-packaged binaries work](#how-pre-packaged-binaries-work).

#### Thread safety

V1 ships **NTS binaries only**. PIE will not match a ZTS PHP installation (`composer.json` declares `"support-zts": false`). This is deliberate: the extension keeps a process-global runtime and connection registry, and TSRM per-thread isolation is not implemented in V1, so ZTS binaries would share that registry across PHP threads without synchronization. The previous advisory ZTS CI job (`continue-on-error`) only proved that a ZTS binary loads, not that it is safe under real concurrency. ZTS support is planned for V2 with per-thread isolation, a blocking ZTS CI job, and real concurrency tests — tracked in [`docs/plans/ROADMAP.md`](plans/ROADMAP.md) (Parked — ZTS).

### Step 1 — Install the native extension

```bash
pie install goopil/rabbit-rs-native
```

PIE selects the correct pre-compiled binary for your environment:

- PHP version (8.4 or 8.5)
- Architecture (x86_64 or arm64)
- libc (glibc or musl)
- Thread safety (NTS only in V1)

It copies the shared object (`rabbit_rs.so`) to your PHP extension directory and enables it in the active PHP configuration.

#### Verify installation

```bash
php --ri rabbit_rs
```

Expected output:

```
rabbit_rs

Rabbit RS - High-performance RabbitMQ transport for PHP and Laravel, powered by Rust
Version => 0.1.2
...
```

#### Dockerfile usage

Use PIE in a multi-stage Dockerfile. No dedicated Rabbit RS image is needed:

```dockerfile
FROM php:8.4-cli AS base

# Install PIE
RUN curl -L https://github.com/php/pie/releases/latest/download/pie.phar -o /usr/local/bin/pie \
    && chmod +x /usr/local/bin/pie

# Install the extension
RUN pie install goopil/rabbit-rs-native

# Verify
RUN php --ri rabbit_rs

# Install Composer and the Laravel queue driver
COPY --from=composer:latest /usr/bin/composer /usr/bin/composer
RUN composer require goopil/rabbit-rs-laravel

# ... your application
```

For a complete Dockerfile example, see [examples/laravel/Dockerfile](../examples/laravel/Dockerfile).

### Step 2 — Install the Laravel queue driver

```bash
composer require goopil/rabbit-rs-laravel
```

Composer installs the PHP package. It does **not** install or modify system PHP binaries — that is PIE's job — and it does not verify the extension either: `ext-rabbit_rs` is a Composer *suggestion* (`^0.2.2`), so `composer install` succeeds without it, and a connection resolved without the extension (or with a version outside the constraint) fails at connection resolution with a typed error naming the install command (see [Why Composer doesn't modify system PHP](#why-composer-doesnt-modify-system-php)).

The package auto-discovers the service provider in Laravel 12 and 13. If you disabled auto-discovery, register it manually:

```php
// config/app.php
'providers' => [
    // ...
    Goopil\RabbitRs\Laravel\RabbitMqServiceProvider::class,
],
```

### Step 3 — Publish the configuration

```bash
php artisan vendor:publish --tag="rabbit-rs-config"
```

This creates `config/rabbit-rs.php` with sensible defaults. See [Configuration](../packages/laravel-queue/docs/reference.md#configuration) for the full reference.

### Step 4 — Verify the installation

```bash
php artisan rabbit-rs:status
```

This displays connection state, pool metrics, and consumer stats. For machine-readable output:

```bash
php artisan rabbit-rs:status --format=json
```

### Local compilation with Cargo

For contributors or environments without PIE:

```bash
# Clone the repository
git clone https://github.com/Goopil/php-rabbit-rs.git
cd php-rabbit-rs

# Build the extension in release mode
cargo build --release -p rabbit-rs-php

# Install into the current PHP
./scripts/install.sh --release
```

The `install.sh` script wraps `cargo php install` with the correct manifest path (the workspace root `Cargo.toml` is workspace-only, so `cargo-php` needs the package manifest at `crates/rabbit-rs-php/Cargo.toml`).

#### Requirements for local compilation

- Rust 1.96.0 (pinned in `rust-toolchain.toml`)
- `cargo-php` (install with `cargo install cargo-php`)
- PHP 8.4 or 8.5 with development headers
- `libssl-dev` (or `openssl-devel` / `openssl-dev` depending on your distro)

### Why Composer doesn't modify system PHP

The native extension is a binary shared object (`rabbit_rs.so`) that must be compiled for your specific PHP version, architecture, libc, and thread-safety mode. Composer is a PHP dependency manager — it handles PHP source packages, not system binaries.

The separation is:

| Tool | Responsibility |
|------|---------------|
| PIE | Downloads and installs the correct pre-compiled `.so` binary |
| Composer | Installs the Laravel queue driver (PHP source); `ext-rabbit_rs` stays a suggestion — connections fail at connection resolution until the extension is loaded |

The Laravel driver's `composer.json` declares `ext-rabbit_rs` as a *suggestion* (`^0.2.2`), not a requirement: `composer install` succeeds without the extension. The constraint is enforced at connection resolution — the driver fails with a typed error when the extension is missing or its version falls outside the constraint. Composer never installs the binary — that is PIE's role.

### Multiple PHP versions

If you have multiple PHP installations, PIE and `cargo-php` target the PHP found in your `PATH`. To target a specific PHP, run them with that PHP's interpreter and ensure its `php-config`/`phpize` come first in the `PATH`:

```bash
# With PIE (uses the php-config/phpize in PATH)
/path/to/php/bin/php /usr/local/bin/pie install goopil/rabbit-rs-native

# With cargo-php (php-config of the target PHP first in PATH)
PATH="/path/to/php/bin:$PATH" ./scripts/install.sh --release
```

### Upgrading and rollback

PIE installs are versioned replacements: installing a different version swaps the `rabbit_rs.so` binary and updates the active PHP configuration in place. Upgrades and rollbacks use the same `pie install` command with an explicit release tag.

Upgrade (or reinstall) an exact version:

```bash
pie install goopil/rabbit-rs-native:v0.1.1
```

Rollback = install the previous tag:

```bash
pie install goopil/rabbit-rs-native:v0.1.0
```

Check which version is active before and after:

```bash
php --ri rabbit_rs
```

Keep the Laravel queue driver in sync: `goopil/rabbit-rs-laravel` tracks a specific `ext-rabbit_rs` constraint (`^0.2.2`). When moving across a version boundary — in either direction — upgrade or roll back the extension and the driver together. Composer cannot check loaded extensions (the constraint lives in `suggest`), so the driver enforces the constraint itself with a typed error at connection resolution: a half-upgraded system (new driver with old extension, or the reverse) fails loudly on the first connection instead of going unnoticed.

Every release exercises these paths in CI before it is finalized: the release pipeline installs the previous published release, upgrades it to the new release, and rolls back again (see [End-to-end PIE validation](#end-to-end-pie-validation)).

### Distribution model

Rabbit RS distributes two packages in synchronized releases:

- **`goopil/rabbit-rs-native`** — the native PHP extension, installed via [PIE](https://github.com/php/pie)
- **`goopil/rabbit-rs-laravel`** — the Laravel queue driver, installed via [Composer](https://getcomposer.org)

Both packages share the same version number: a release `1.2.0` produces `goopil/rabbit-rs-native 1.2.0` and `goopil/rabbit-rs-laravel 1.2.0`. The Laravel package suggests `ext-rabbit_rs ^0.2.2` — the constraint tracks the extension version until 1.0 and is enforced by a typed error at connection resolution (see [Why Composer doesn't modify system PHP](#why-composer-doesnt-modify-system-php)).

#### PIE build matrix

The CI produces **8 pre-compiled release artifacts** (V1 is NTS-only, see [Thread safety](#thread-safety)) covering all supported combinations:

| PHP | Architecture | libc | Thread safety |
|-----|-------------|------|---------------|
| 8.4 | x86_64 | glibc | NTS |
| 8.4 | x86_64 | musl | NTS |
| 8.4 | arm64 | glibc | NTS |
| 8.4 | arm64 | musl | NTS |
| 8.5 | x86_64 | glibc | NTS |
| 8.5 | x86_64 | musl | NTS |
| 8.5 | arm64 | glibc | NTS |
| 8.5 | arm64 | musl | NTS |

The matrix is defined in [`release/pie-matrix.json`](../release/pie-matrix.json).

#### Functional coverage of the matrix

Every cell above is compiled, load-smoked, checksum-verified, and attested by the release pipeline — but "loads" is not "works against a broker". Real-broker coverage (publish + confirms, consume + ack, one Toxiproxy outage/recovery scenario via [`scripts/amqp-smoke.sh`](../scripts/amqp-smoke.sh), issue #227) is narrower and proven per tier:

| PHP | Architecture | libc | Functional proof |
|-----|-------------|------|------------------|
| 8.4 | x86_64 | glibc | CI `integration` job (full Rust + Laravel integration suites) and the release PIE install smoke |
| 8.4 | x86_64 | musl | Nightly [functional matrix](../.github/workflows/functional-matrix.yml) docker cell and the release PIE install smoke |
| 8.4 | arm64 | glibc | Nightly functional matrix docker cell and the release PIE install smoke |
| 8.4 | arm64 | musl | Nightly functional matrix docker cell and the release PIE install smoke |
| 8.5 | x86_64 | glibc | Nightly functional matrix (full integration suites + AMQP smoke) and the release PIE install smoke |
| 8.5 | x86_64 | musl | Build-only (shares the musl runtime path proven by the 8.4 musl smoke) |
| 8.5 | arm64 | glibc | Build-only (shares the glibc runtime path proven by the 8.5 glibc x86_64 integration) |
| 8.5 | arm64 | musl | Build-only (shares the musl runtime path proven by the 8.4 musl smoke) |
| 8.4 | arm64 | darwin | Nightly functional matrix `macos-arm64` cell (dispatch/manual only): load + publish/confirm + consume/ack against a local Homebrew rabbitmq — no recovery scenario (no Toxiproxy on macOS runners) |
| 8.5 | arm64 | darwin | Build-only (shares the 8.4 macOS ARM64 functional cell) |

"Build-only" means the release pipeline compiles the artifact for that cell and proves it loads with the target PHP, but no AMQP traffic is exercised there; the runtime path it shares with a functional sibling is the only broker-level evidence. The functional bar itself is deliberately small — extension load, 5 messages published and confirmed, consumed and acked with the queue drained, and one outage/recovery scenario (publish 2 into a disabled Toxiproxy proxy, require both to buffer and confirm after the network heals) — so a release asset that cannot deliver against a real broker fails somewhere in CI, not in production.

#### How pre-packaged binaries work

Each artifact is a ZIP archive containing a single `rabbit_rs.so` compiled for the exact combination of PHP version, architecture, libc, and thread-safety mode. The naming convention follows PIE's expected format, which includes the `v` prefix from the git tag:

```
php_rabbit_rs-v{version}_php{php}-{arch}-linux-{libc}-{ts}.zip
```

For example:

```
php_rabbit_rs-v1.2.0_php8.5-x86_64-linux-glibc-nts.zip
```

Every Linux artifact carries an **explicit** thread-safety suffix (`-nts` in V1). PIE (1.4.10+, the version the release pipeline validates) resolves NTS assets matched either with or without the `-nts` suffix (and requires `-zts` for ZTS builds, planned for V2); the explicit suffix is the repository convention so that asset names are unambiguous and self-describing. The convention is enforced in two places that must stay consistent:

- `release/pie-matrix.json` — machine-readable matrix (`ts_suffix` is always `-nts` in V1; ZTS entries are excluded)
- `.github/workflows/release.yml` — release build (`-${{ matrix.ts }}` appended to every asset name) via the `.github/actions/package-release-asset` composite action

`scripts/validate-distribution.sh` fails if any of them drifts from the pattern expected by PIE.

macOS artifacts (`arm64-darwin-nts`) are outside the PIE matrix — `composer.json` declares `os-families: ["linux"]` — and are consumed by the Homebrew formula. macOS installs are therefore validated best-effort: the release pipeline compiles and smoke-loads both `arm64-darwin-nts` builds, and the Homebrew formula is audited and install-tested on a macOS ARM64 runner by [`homebrew-formula-test.yml`](../.github/workflows/homebrew-formula-test.yml) (formula-affecting pull requests and manual dispatch). The release pipeline itself does not reinstall through Homebrew on macOS — if that install path regresses, the formula test workflow fails on its next run rather than blocking the release.

#### End-to-end PIE validation

Once the release is published, the release pipeline blocks on two verification stages against the published release:

- **`verify-pie-install`** runs a real `pie install` on every supported platform/libc combination: Linux glibc x86_64 (PHP 8.4 and 8.5) on native runners, and Linux glibc arm64, musl x86_64, and musl arm64 (PHP 8.4) inside the same digest-pinned, multi-arch PHP images the build job uses. Each job asserts that `rabbit_rs` loads and that `phpversion('rabbit_rs')` equals the release version.
- **`verify-pie-upgrade-rollback`** installs the previous published release, upgrades it to the new release, then rolls back to the previous one, asserting the installed extension version at each step. A missing previous release fails the job loudly instead of silently skipping the gate.

The Homebrew formula update and the Laravel package split run only after both stages succeed, so a release that PIE cannot install, upgrade, or roll back never reaches those channels.

Each release archive is accompanied by:

- A **SHA-256** checksum file (`.sha256`)
- A **CycloneDX SBOM** in JSON format (`.sbom.json`), generated from the `rabbit-rs-php` crate via `cargo-cyclonedx` 0.5.9
- A **GitHub build provenance attestation** (SLSA v1), signed with Sigstore using the workflow's OIDC identity
- A **GitHub SBOM attestation** binding the SBOM to the ZIP artifact

Attestations are stored in the GitHub attestations API and verified with:

    gh attestation verify <asset.zip> --repo Goopil/php-rabbit-rs \
        --predicate-type https://slsa.dev/provenance/v1

Each release therefore contains **30 assets**: 10 ZIPs, 10 SHA256 files, and 10 SBOM files, plus 20 attestations (provenance + SBOM) stored in the attestations API (not listed as release assets).

#### Build characteristics

- **Static linking** — Rust dependencies and TLS libraries are linked statically into the `.so` whenever possible; the only expected system dependency is **libc** (glibc or musl). No Rust, OpenSSL, or other runtime library is needed on production systems.
- **Minimum glibc 2.31** — covers Debian 11 and later, Ubuntu 20.04 and later, CentOS 9 Stream and later. Alpine is not affected (uses musl). On an older glibc, use the musl build or compile from source.
- **No debug builds** — every release artifact is a release-optimized build, ensuring consistent performance and avoiding debug-assertion overhead.

#### Release synchronization

Releases follow a strict order to ensure version coherence:

1. **CI builds all 8 PIE artifacts** — each tested with the target PHP, checksum verified
2. **Laravel package is split** — the monorepo's `packages/laravel-queue/` is split into the `Goopil/rabbit-rs-laravel` mirror repository via `scripts/split-laravel-package.sh`
3. **Native extension is tagged on Packagist** — `goopil/rabbit-rs-native` appears as a PIE package
4. **Laravel package is tagged on Packagist** — `goopil/rabbit-rs-laravel` appears as a Composer package
5. **GitHub release is published** — only after all binaries are produced, the Laravel tag is pushed, and both Packagist metadata entries are verified

The validation script [`scripts/validate-distribution.sh`](../scripts/validate-distribution.sh) checks: root package name and type (`goopil/rabbit-rs-native`, `php-ext`), extension name, download method, NTS support with the V1 ZTS exclusion, Linux-only OS family, Laravel package name and namespace, version coherence between Cargo and both packages, exactly 8 PIE matrix entries (NTS only) with unique suffixes, the minimum glibc version, the PIE asset naming convention across the packaging script, the release workflow and this document, and — when archives are present — exactly 30 files (10 ZIP + 10 SHA-256 + 10 SBOM for the 8 Linux matrix entries plus 2 macOS darwin assets) with verified checksums and CycloneDX SBOMs.

#### Not V1 distribution channels

| Channel | Status | Alternative |
|---------|--------|-------------|
| PECL | Not supported | Use `pie install goopil/rabbit-rs-native` |
| Debian packages (apt) | Not maintained | Use PIE in your Dockerfile |
| RPM packages (dnf/yum) | Not maintained | Use PIE in your Dockerfile |
| APK packages (Alpine) | Not maintained | Use PIE (musl binary works on Alpine) |
| Composer plugins installing binaries | Not used | PIE handles binaries, Composer handles PHP source |
| Full PHP images bundling the extension | Not provided | Install PIE in your own Dockerfile |

This is a deliberate design decision: PIE is the PHP ecosystem's official extension installer and handles the binary dimension correctly (version matching, architecture selection, activation). Keeping binary distribution in PIE and source distribution in Composer maintains a clean separation of concerns.

### Next steps

- [Configuration reference](../packages/laravel-queue/docs/reference.md#configuration)
- [Laravel usage](https://github.com/Goopil/php-rabbit-rs/blob/main/packages/laravel-queue/docs/reference.md#usage)
- [Topology management](../packages/laravel-queue/docs/reference.md#topology)
- [Reliability](#reliability)

## Reliability

Rabbit RS provides at-least-once delivery. Once a message is accepted into the confirmed delivery path, silent loss is unacceptable; duplicates are permitted and must remain identifiable and measurable. The guarantee is scoped to the live process: the publisher replay buffer is process memory, and a PHP crash can drop publications the broker never received — see [What the replay buffer is not](#what-the-replay-buffer-is-not).

The documented exception is `safety = blind`: an explicit fire-and-forget mode (silent loss possible), set per connection (`safety` key) or package-wide (env `RABBIT_RS_SAFETY`) as well as in the raw native extension configuration. Publisher confirms and mandatory routing are **derived from `safety`** by the `ConnectionCompiler` — there are no separate `confirms`/`mandatory` config keys; see [Configuration — Safety modes](../packages/laravel-queue/docs/reference.md#safety-modes).

### At-least-once contract

The delivery contract is:

- **No silent loss** — every publish on the confirmed delivery path (`safe` mode) is either confirmed or the caller is notified of failure
- **Duplicates permitted** — in failure windows, the same message may be delivered more than once
- **Duplicates identifiable** — each message carries a stable `message_id` (UUID from Laravel payload)
- **Duplicates measurable** — metrics track redeliveries and duplicate counts

This means your jobs **must be idempotent**. Use the `message_id` to detect and handle duplicates at the application level.

### Publisher confirms

Publisher confirms are **enabled by default** (`safety = "safe"`, the default, derives `confirms = true`). When enabled:

1. The publisher calls `confirm.select` on the channel
2. Each published message is assigned a sequence number
3. The broker sends `basic.ack` (confirmed) or `basic.nack` (rejected) with the sequence number
4. The publish call resolves once its confirm is received. Batched publishes are **pipelined**: the call returns before confirmations resolve, and unconfirmed outcomes surface at the next operation (the next publish flush, `drainSettlementErrors()`, or `stats()`)

A confirm timeout (connection key `confirm_timeout`, default 30000 ms) ensures the call does not hang indefinitely. During a recovery, a publish parked in replay is retried once with a fresh deadline; a confirm timeout on a live connection stays terminal (unknown outcome → no automatic resend).

### Mandatory returns

Mandatory routing is **always on in safe mode** (`safety = "safe"`, which derives `mandatory = true`). When enabled:

- The broker returns unroutable messages via `basic.return` instead of silently dropping them
- `basic.return` is processed **before** the corresponding `basic.ack` — a return takes precedence over a following ACK
- The publish call resolves with a `Returned` outcome, and the Laravel queue driver throws a `QueueException`

There is no separate `mandatory` config key: it is derived from `safety` (`safe` → confirms + mandatory, `unsafe` → neither — a synchronous socket write without outcome tracking, `blind` → neither — fire-and-forget through a bounded pump) — a connection key named `mandatory` hits the unknown-key rejection with an actionable error. The only mode with mandatory routing is `safe`.

#### Delayed publishes

Publications carrying the `x-delay` header (delay plugin mode) are **never mandatory** on the wire, even in safe mode: the `rabbitmq_delayed_message_exchange` plugin returns every mandatory delayed publish as unroutable (`basic.return`), so mandatory routing is suppressed for plugin-routed delayed messages. Publisher confirms remain enabled — the publish still resolves only after the broker confirm, and the no-silent-loss contract is preserved by the confirmed `rabbit-rs.delayed` binding (see [Topology — Delay routing](https://github.com/Goopil/php-rabbit-rs/blob/main/packages/laravel-queue/docs/reference.md#delay-routing)). Delayed messages published through TTL bucket queues carry no `x-delay` header on the wire and remain mandatory.

### Connection recovery

Rabbit RS handles connection loss automatically. The connection states are `Disconnected`, `Connecting`, `Ready`, `Recovering`, `FailedPermanent`, and `Closed`:

```
Disconnected → Connecting → Ready → Recovering → Ready
                 |                       |
                 +→ FailedPermanent ←----+
             (permanent errors: authentication failure,
              incompatible topology)

close() → Closed (from any state)
```

#### Recovery sequence

Recovery follows a **deterministic order**:

1. **Connection** — re-establish the TCP connection and AMQP negotiation (the generation increments)
2. **Publisher channel** — open a fresh publisher channel
3. **Topology** — declare or verify exchanges, then queues, then bindings
4. **Publisher replay** — replay unconfirmed publications from the bounded buffer
5. **Consumers** — per subscription: open the consumer channel, re-apply QoS, re-register `basic.consume`

This order ensures that consumers are only re-registered after their queues and bindings exist, and that publishers resume — replay included — after the topology is restored, before consumers re-register.

With multiple brokers, each broker recovers independently through its own coordinator: one broker recovering never blocks consumption from the others. When a broker's consumer set is replaced after recovery, the composed multi-broker consumer surfaces a one-shot `ConnectionException` ("broker source replaced by recovery; re-fetch consumer") — re-fetch the consumer to resume deliveries from that broker (see [Multiple brokers and vhosts](../packages/laravel-queue/docs/reference.md#multiple-brokers-and-vhosts)).

#### Backoff

Retries use exponential backoff with jitter:

- Initial backoff: 100 ms
- Multiplier: 2x
- Maximum: 30 seconds
- Jitter: 20%

Permanent errors (authentication failures, incompatible topology) are not retried in publish contexts. Consumer workers may continue retrying according to their own policy.

### Delivery tokens and stale ACK rejection

Each delivery carries an opaque token containing:

- Connection identity
- Channel ID
- Consumer tag
- Delivery tag
- **Connection generation**

After a connection recovery, the generation increments. If the PHP code attempts to ACK a delivery from an old generation, the extension **rejects the stale ACK**. RabbitMQ redelivers the message.

This handles the race condition where:
1. A job is delivered to PHP
2. The job completes, but the ACK hasn't reached the broker
3. The connection drops
4. The connection recovers (new generation)
5. PHP attempts to ACK the old delivery
6. The extension rejects the stale ACK
7. RabbitMQ redelivers the message

The job may be executed twice. This is expected and why jobs must be idempotent.

Closing a consumer (or its pool) flushes pending and queued acknowledgements to the broker within a bounded 500 ms budget before the channels close; settlements still unacknowledged after the budget are abandoned to redelivery, preserving at-least-once.

### Replay buffer

When a connection drops before a publish is confirmed, the state is ambiguous — the broker may or may not have received the message. Rabbit RS handles this by:

1. **Classifying unconfirmed publications as ambiguous** — the publish call does not resolve immediately
2. **Placing them in a bounded in-memory replay buffer** — with the same `message_id`, payload, destination, and original deadline
3. **Replaying them after recovery** — once the topology is restored and a new confirm-enabled channel is open
4. **Reusing the original deadline** — the deadline is never reset by a reconnection

The replay buffer is **bounded** by the publisher's global buffer capacity — a shared budget for in-flight confirms and replayed publications (1024 publications and 64 MiB of buffered payload bytes by default). When the budget is exhausted, new publications receive `Backpressure` instead of being accepted.

#### What the replay buffer is not

The replay buffer is **in-memory only**. It survives connection drops but **not** a PHP process crash. If the PHP process crashes, all unconfirmed publications in the buffer are lost.

For durability beyond a process crash, use an **external outbox** pattern:

1. Write the job to a persistent store (database) within the same transaction as your business operation
2. A separate process reads from the outbox and publishes to RabbitMQ
3. Delete the outbox entry after a publisher confirm

Rabbit RS does not include an outbox in V1. The in-memory replay buffer covers the common case of transient network failures.

### Duplicates

Duplicates are expected and normal. They occur in these scenarios:

| Scenario | Cause |
|----------|-------|
| Connection drop after delivery, before ACK | Stale ACK rejected, RabbitMQ redelivers |
| Connection drop after publish, before confirm | Replay buffer republicates; broker may have received the original |
| Worker crash with in-flight jobs | RabbitMQ redelivers unacked messages |

#### Handling duplicates

1. **Make jobs idempotent** — use `message_id` or business keys to detect duplicate work
2. **Use `attempts()`** — the `RabbitMqJob::attempts()` method returns the delivery count from `x-acquired-count` or `x-delivery-count` headers
3. **Set `delivery_limit`** — quorum queues dead-letter messages that exceed the limit; `dead_letter` must be configured when `delivery_limit` is set
4. **Monitor duplicates** — see *Measuring duplicates* below; cross-process `messages_redelivered` is the observable duplicate signal

#### Measuring duplicates

Native pool metrics — including `duplicates_total` — are **per-process by design**: they count deliveries that the broker flagged as redeliveries and that *that* process settled. Every worker process has its own counters, and `rabbit-rs:status` creates a brand-new pool inside the artisan CLI process, so the native metrics it prints are **same-process only** and read zero in a fresh CLI process.

`php artisan rabbit-rs:status` therefore observes two distinct things:

**Native pool metrics (same-process only).** Useful from inside a long-lived process; always zeros in a fresh CLI process:

- `reconnects_total` — number of connection recoveries (each can cause duplicates)
- `deliveries_total` — total deliveries received
- `acks_total` / `rejects_total` — settlement counts
- `duplicates_total` — deliveries the broker flagged as redeliveries, settled by this process

**Queue counters (cross-process).** Set the optional per-connection key `queue.connections.<name>.management_url` (e.g. `http://broker-host:15672`) and the status command fetches per-queue counters from the RabbitMQ management API, across every process touching the queue:

- `messages_delivered` / `messages_acked` — broker-side delivery and settlement totals
- `messages_redelivered` — an **approximate duplicate signal**: at-least-once also redelivers after a consumer crash, so a redelivery is not necessarily a duplicate. Watch for sustained growth correlated with reconnects.

An in-process Prometheus exporter is the planned evolution for per-process counters; it is deliberately not provided today.

### When to use an external outbox

Use an external outbox when:

- You need durability across PHP process crashes (not just connection drops)
- You publish within a database transaction and need the publish to be transactional with the database write
- You cannot tolerate any message loss, even in the ambiguous window

Without an outbox, the in-memory replay buffer covers transient network failures but not process crashes. For most Laravel applications, the default behavior is sufficient — PHP workers are typically supervised by Supervisor or Kubernetes and restart automatically.

### Panic policy

Rabbit RS runs as a native PHP extension: an uncaught Rust unwind crossing the FFI boundary aborts the whole PHP process. The core therefore keeps panics out of every code path reachable from a PHP operation, and routes diagnostics through the log facade (`rabbit_rs_core::log`) instead of stderr.

1. Production code must not call `unwrap()`, `expect()`, or the `panic!` family on paths reachable from a PHP operation. Prefer typed errors with actionable context.
2. A remaining `expect`/`unwrap` is accepted only as a documented, proven invariant — one that cannot fire without a prior logic bug in the same synchronous block (see the audit below).
3. Panics in `#[cfg(test)]` code are out of scope; tests may panic freely.
4. Background Tokio tasks must terminate cleanly on failure (log through the facade, then return) instead of panicking inside a spawned task.

### Log facade

- The core depends on no logging framework and never writes to stderr.
- Embedders install one process-wide sink (`rabbit_rs_core::log::install`); the first installation wins and later calls are rejected, which keeps forks and repeated initializations deterministic.
- Without an installed sink the core is silent; records emitted before the first install are dropped, so install at startup before spawning pools.
- Redaction contract: call sites only log broker names, connection generations, and transport error messages — never credentials, complete broker URIs, or private certificate material. Sinks must preserve this when forwarding.

### Panic audit (2026-09-01, issue #56)

`rg -n 'unwrap\(\)|expect\(|panic!|unreachable!|todo!|unimplemented!'` over `crates/rabbit-rs-core/src` and `crates/rabbit-rs-php/src`, restricted to production code (`#[cfg(test)]` modules excluded).

#### Fixed in this round

| Site | Problem | Resolution |
| --- | --- | --- |
| `pool/recovery_coordinator.rs` `wait_for_state` | `expect` on the state watch when the coordinator task had stopped: panic reachable from PHP-facing waits | Returns `ConnectionState::Closed` when the watch dies; `state()` also reports `Closed` for a dead watch so pool loops observe a terminal state |
| `pool/recovery_coordinator.rs` `run_coordinator` | `expect("connection actor started")` inside a spawned task | Logs through the facade (`Level::Error`) and terminates the task cleanly |
| `pool/recovery_coordinator.rs` recovery failure | `eprintln!` leaked diagnostics to stderr | Routed through the log facade (`Level::Warn`), carrying the typed `CoordinatorError` |

#### Accepted invariants (documented, no runtime conversion)

| Site | Invariant |
| --- | --- |
| `client.rs` `topology_plan()` fallback `.expect("external mode always compiles")` | `TopologyPlan::compile` on an empty `External`-mode plan validates nothing and cannot fail; the expect guards a compile-time-true property |
| `consumer/actor.rs` `drain_pending` `.expect("front checked above")` | `pop_front` runs only after `front()` returned `Some` in the same synchronous block with no mutation in between |
| `consumer/attempts.rs` `DEFAULT_MAX_ATTEMPTS_NON_ZERO` | Const-evaluated `match`; the `panic!` fires at compile time if the constant is ever zero, never at runtime |
| `topology/delay.rs` `write!(...).expect("writing to String is infallible")` | `fmt::Write for String` cannot fail |

#### PHP extension (`crates/rabbit-rs-php`)

`callbacks.rs` (callback registry), `classes/bridge.rs`, and `classes/publish_buffer.rs` call `.expect("... mutex poisoned")` on `std::sync::Mutex` locks. A poisoned mutex requires a prior panic while the lock was held; the critical sections in these types perform no panicking operations, so the poison state is unreachable in practice. These sites stay documented rather than converted: converting them would trade a proven invariant for error propagation through FFI paths that have no meaningful recovery.

### Error typing

`CoordinatorError` is a typed enum (`Topology`/`Transport`/`Publisher`/`Consumer`/`Internal`) whose variants carry the typed source error; `Display` messages keep the previously surfaced context. Callers must classify through variants, never through string matching.

### TLS

The AMQP transport always verifies the broker certificate (rustls, `verify: peer` — the only accepted value, and the default). A custom `ca_cert` chain extends the platform trust store, and a client identity (`client_cert` + `client_key`) enables mTLS: it is supported and certified by the lab test suite, which proves a connection with a client certificate succeeds against a broker listener that rejects anonymous clients (`fail_if_no_peer_cert = true`), and that the same listener rejects connections without one.

TLS server name indication (SNI) and certificate hostname verification always use the AMQP connection host. A `server_name` override is not possible with the underlying AMQP transport (lapin 4.10) and is rejected at validation when it differs from the first configured host — a documented gap tracked for post-1.0 (#164, #166).

## Troubleshooting

### Common errors and solutions

#### Extension not loaded

**Error:**
```
The Rabbit RS Laravel driver requires ext-rabbit_rs ^0.2.2 to be loaded.
```

**Solution:**

Install the native extension:

```bash
pie install goopil/rabbit-rs-native
```

Verify it is loaded:

```bash
php -m | grep rabbit_rs
php --ri rabbit_rs
```

If the extension is installed but not loaded, check your PHP configuration:

```bash
# Find the PHP config directory
php --ini

# Check if the extension is enabled
grep rabbit_rs /path/to/php.ini
# Should show: extension=rabbit_rs
```

#### Connection failures

**Error:**
```
ConnectionException: Failed to connect to broker
```

**Diagnosis:**

1. Verify RabbitMQ is reachable:

```bash
# Check TCP connectivity
nc -zv rabbit-host 5672

# Check AMQP handshake (if rabbitmqadmin is installed)
rabbitmqadmin --host=rabbit-host --port=5672 list vhosts
```

2. Check credentials and vhost:

```bash
# Verify vhost exists
rabbitmqctl list_vhosts | grep '/your-vhost'

# Verify user permissions
rabbitmqctl list_permissions -p /your-vhost
```

3. Check TLS configuration:

```bash
# Test TLS connection
openssl s_client -connect rabbit-host:5671 -CAfile /path/to/ca.pem
```

4. Check the status command:

```bash
php artisan rabbit-rs:status
```

**Common causes:**

| Cause | Solution |
|-------|----------|
| Wrong host/port | Check `RABBIT_RS_HOSTS` env var |
| Wrong vhost | Check `RABBIT_RS_VHOST` — vhosts are case-sensitive |
| Wrong credentials | Check `RABBIT_RS_USERNAME` and `RABBIT_RS_PASSWORD` |
| TLS mismatch | Ensure `RABBIT_RS_TLS=true` when broker requires TLS |
| Firewall | Ensure port 5672 (or 5671 for TLS) is open |
| RabbitMQ not running | Start the RabbitMQ service |

#### Topology errors

**Error:**
```
PRECONDITION_FAILED - inequivalent arg 'x-queue-type' for queue 'orders'
```

**Cause:** The queue exists with different arguments than what Rabbit RS is trying to declare.

**Solutions:**

1. **Use `verify` mode** to check without modifying:

```bash
RABBIT_RS_TOPOLOGY_MODE=verify
```

2. **Use `external` mode** if an external system manages topology:

```bash
RABBIT_RS_TOPOLOGY_MODE=external
```

3. **Delete and recreate the queue** (data loss — use with caution):

```bash
rabbitmqctl delete_queue orders
```

4. **Align your config** with the existing queue arguments (type, durability, delivery_limit).

**Error:**
```
NOT_FOUND - no exchange 'laravel.jobs'
```

**Cause:** In `external` or `verify` mode, the exchange does not exist.

**Solution:** Switch to `declare` mode, or create the exchange manually:

```bash
rabbitmqadmin declare exchange name=laravel.jobs type=direct durable=true
```

#### Permission errors

**Error:**
```
ACCESS_REFUSED - access to queue 'orders' refused
```

**Cause:** The configured user lacks permissions on the vhost or queue.

**Solution:**

```bash
# Grant permissions
rabbitmqctl set_permissions -p /your-vhost username ".*" ".*" ".*"

# For read-only (verify mode)
rabbitmqctl set_permissions -p /your-vhost username "^amq\.|^laravel\." "^amq\.|^laravel\." ".*"
```

**Error:**
```
ACCESS_REFUSED - access to vhost '/production' refused
```

**Cause:** The user does not have access to the vhost.

**Solution:**

```bash
# Grant vhost access
rabbitmqctl set_vhost_permissions -p /production username ".*" ".*" ".*"
```

#### Recovery diagnostics

**Symptom:** The worker reconnects frequently (high `reconnects_total`).

**Diagnosis:**

```bash
php artisan rabbit-rs:status
```

Check:
- `reconnects_total` — if increasing, the connection is unstable
- `backpressure_total` — if high, the broker may be overloaded
- `confirmation_latency_p99` — if high, the broker is slow to confirm

**Common causes:**

| Cause | Diagnostic | Solution |
|-------|------------|----------|
| Heartbeat timeout | Check `RABBIT_RS_HEARTBEAT` (default 30s) | Increase heartbeat or check network |
| Network instability | Check for packet loss, DNS issues | Fix network or use direct IP |
| Broker overload | Check RabbitMQ management UI | Scale RabbitMQ or reduce publish rate |
| Firewall idle timeout | Connection drops after N seconds of idle | Reduce heartbeat below the idle timeout |

**Symptom:** Messages are redelivered after recovery.

**This is expected behavior.** When a connection drops, RabbitMQ redelivers unacked messages. The worker may process the same message twice. Ensure your jobs are idempotent. See [Reliability — Duplicates](#duplicates).

#### Debug logging

Enable debug logging by listening to events:

```php
// In a service provider or EventServiceProvider
use Goopil\RabbitRs\Laravel\Events\ConnectionStateChanged;
use Goopil\RabbitRs\Laravel\Events\BackpressureDetected;
use Illuminate\Support\Facades\Log;

Event::listen(ConnectionStateChanged::class, function (ConnectionStateChanged $event) {
    Log::debug("Rabbit RS: broker {$event->broker} → {$event->state} (gen {$event->generation})");
});

Event::listen(BackpressureDetected::class, function (BackpressureDetected $event) {
    Log::warning("Rabbit RS: backpressure on {$event->broker}: {$event->inFlight}/{$event->capacity}");
});
```

#### Queue depth monitoring

Check queue depth:

```bash
# Via Rabbit RS
php artisan tinker
>>> Queue::connection('rabbit-rs')->size('orders.high')

# Via rabbitmqctl
rabbitmqctl list_queues -p /your-vhost name messages
```

#### Stale ACK rejection

**Symptom:** Log shows stale generation warnings during recovery.

**This is expected behavior.** After a connection recovery, ACKs from the old generation are rejected to prevent double-settlement. RabbitMQ redelivers the message. The job may execute twice — ensure idempotency. See [Reliability — Stale ACK rejection](#delivery-tokens-and-stale-ack-rejection).

#### Backpressure

**Symptom:** `BackpressureException` thrown during publish.

**Cause:** The publisher's bounded capacity is full (in-flight confirms + replay buffer).

**Solutions:**

1. Reduce publish rate — batch jobs, add delays
2. Scale consumers — more workers drain queues faster
3. Check broker health — high confirmation latency indicates broker saturation
4. Scale the publisher buffer — the publisher's bounded capacity (1024 publications by default) is currently **not configurable**: it is not exposed through the Laravel package config (`config/rabbit-rs.php`), and the raw native extension configuration does not accept a `buffer_capacity` key either. Until it is plumbed through, the mitigations above are the only levers.

See [Operations — Backpressure](../packages/laravel-queue/docs/reference.md#backpressure-detection-and-response).

#### Delayed messages not arriving

**Symptom:** `later()` jobs are not delivered after the delay.

**Diagnosis:**

1. Check the delay mode:

```bash
RABBIT_RS_DELAY_MODE=auto
```

2. If using `plugin` mode, verify the plugin is installed:

```bash
rabbitmq-plugins list | grep delay
# Should show: rabbitmq_delayed_message_exchange
```

3. If using `ttl` mode, check the TTL queues exist:

```bash
rabbitmqctl list_queues -p /your-vhost name messages | grep delay
```

4. Check the delay buckets — delays are rounded up to the nearest bucket:

```bash
# With buckets [1, 5, 30, 120]
# A 3-second delay → bucket 5 (delivered after ~5 seconds)
```

5. Check the publisher safety mode — in `blind` mode, delayed jobs are published immediately: the pump bypasses delay routing, so `delay_ms > 0` is not honored. Use `safe` or `unsafe` for delay routing:

```bash
RABBIT_RS_SAFETY=safe
```

#### Getting help

If you cannot resolve an issue:

1. Run `php artisan rabbit-rs:status --format=json` and save the output
2. Run `php --ri rabbit_rs` and save the output
3. Check [Reliability](#reliability) for delivery semantics
4. Check the [troubleshooting checklist](https://github.com/Goopil/rabbit-rs/issues) for known issues
5. Open an issue on [GitHub](https://github.com/Goopil/rabbit-rs/issues) with the diagnostic output
