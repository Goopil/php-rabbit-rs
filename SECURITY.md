# Security Policy

## Reporting a vulnerability

**Do not open a public issue for a vulnerability.** Use GitHub's [private vulnerability reporting](https://github.com/Goopil/php-rabbit-rs/security/advisories/new) (Security Advisories). Reports are handled privately; credit is given on request when a fix is published.

## Supported versions

- **Pre-1.0:** only the latest `0.x` release receives security fixes. Earlier releases are not patched — upgrade before reporting.
- **After 1.0:** only the latest `1.x` minor line receives security fixes.

## Response expectations

- **Acknowledgement:** within 3 business days.
- **Fix target:** within 90 days for a confirmed vulnerability.
- **Critical reports** (remote code execution, credential exposure, silent message loss) are expedited ahead of that target.

## Scope

- The Rust core (`crates/rabbit-rs-core`).
- The native PHP extension (`crates/rabbit-rs-php`), including PIE-distributed binaries and the Homebrew formula.
- The Laravel queue driver (`packages/laravel-queue`).
- Release assets: ZIPs, SHA-256 checksums, SBOMs, and attestations. To verify a release asset, follow the existing release-verification instructions in [docs/reference.md](docs/reference.md#end-to-end-pie-validation) (`gh attestation verify` for provenance and SBOM attestations) rather than ad-hoc checks.

Out of scope: vulnerabilities in RabbitMQ itself, PHP, or Laravel — report those to the respective upstream projects.

## Reporting credentials safely

Never paste full AMQP URIs, credentials, or certificate material into a report:

- Redact connection strings: `amqp://user:***@host:5672/vhost`.
- Prefer the redacted output of `php artisan rabbit-rs:doctor` over raw logs or configuration dumps.
- The log-facade redaction contract in [docs/reference.md](docs/reference.md#log-facade) (broker names, connection generations, and transport error messages only) applies to reports too: strip anything beyond that from logs you attach.
