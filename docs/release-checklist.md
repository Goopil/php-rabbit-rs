# Release Checklist (1.0 go/no-go)

Observable-criteria-only gate for the 1.0 release. Every row must cite real
evidence (workflow run link, artifact path, or command output committed to
the release issue) — a checked box without a link is not evidence.

**RC window (decision D5).** Evidence is collected **on the RC tag**: nightly
runs on `main` do not transfer to the RC tag (lockstep releases), so the
soak evidence below must come from nightly runs that checked out the RC ref.
Requirement: **≥7 consecutive green nightly soaks against the RC tag**,
each showing `missing == 0`, terminal `publish_buffered == 0`, a leak slope
within the documented budget, and 100 % reconnect recovery.

**Workflow map.** Pushing an RC tag (`v*-*rc*`) runs the RC pipeline
(`release-candidate.yml`: calls `nightly.yml` + `functional-matrix.yml`, and
runs `scripts/verify-release-candidate.sh` with archived evidence) **and**
the release pipeline (`release.yml`, which triggers on every `v*` tag and
builds + attests the distributable assets). Fill the table from those runs.

## Evidence table

| # | Criterion | Command / workflow | Evidence link | Date | Result |
|---|-----------|--------------------|---------------|------|--------|
| 1 | Fast gate green on the RC commit | `./scripts/check.sh` | <!-- run link --> | | |
| 2 | PR-tier CI green on the RC commit: TLS suite (6 cases), broker-backed FPM, one Octane runtime server, doc lint, composer audit | `.github/workflows/ci.yml` | <!-- run link --> | | |
| 3 | RC orchestrator exits 0 on the RC tag; evidence archived as workflow artifacts | `./scripts/verify-release-candidate.sh` (or the `rc-orchestrator` job of `release-candidate.yml`) | <!-- run link + artifact --> | | |
| 4 | Nightly soak evidence against the RC ref: ≥7 consecutive green runs, each `missing == 0`, terminal `publish_buffered == 0`, leak slope in budget, 100 % reconnect recovery | `soak.yml` re-run against the RC tag (or 7 nightly runs on the tag) | <!-- run links (7) --> | | |
| 5 | Octane certification green for FrankenPHP, RoadRunner, Open Swoole, Swoole (publish → reload → graceful stop → no loss → `publish_buffered == 0`) | `nightly.yml` matrix via `release-candidate.yml` | <!-- run link --> | | |
| 6 | FPM certification green: 2-worker isolation, lone-publish age-flush, graceful-stop flush, reload survival | `ci.yml` `fpm` job; tier 5 of the orchestrator | <!-- run link --> | | |
| 7 | Functional matrix: every cell (PHP 8.4/8.5 × glibc/musl × x86_64/ARM64, macOS ARM64) green or explicitly marked build-only | `functional-matrix.yml` via `release-candidate.yml`; table in `docs/reference.md` | <!-- run link --> | | |
| 8 | Distribution: 30 assets verified; attestations green per asset; PIE install + upgrade/rollback green; each install cell passes `amqp-smoke.sh` | `./scripts/validate-distribution.sh`; `release.yml` `verify-assets` + `verify-pie-install` jobs | <!-- run link --> | | |
| 9 | This checklist complete: versions, dates, evidence links, benchmark comparison within thresholds (tier 9 of the orchestrator, or `benchmarks/baselines/check-budgets.php` on a rebench run) | `release-candidate.yml` artifacts | <!-- link --> | | |
| 10 | Contract coherence: doc lint green; README support table matches `release/pie-matrix.json`, `composer.json`, and the design doc | `./scripts/check-docs.sh` (runs in `ci.yml` `docs` job) | <!-- run link --> | | |
| 11 | Security: `SECURITY.md` live; Dependabot composer active; `composer audit` + `cargo deny check` green | `ci.yml` `composer-audit` + `deny` jobs | <!-- run link --> | | |
| 12 | #221 fixed via refcounted pool claims; the Laravel-side pin removed; sibling-pool isolation tests green | `./scripts/test-fpm.sh`; core + ext test suites in `ci.yml` | <!-- run link --> | | |

## Asset count and attestations

Verification pointers for criterion 8 (run against the assembled `release/`
directory or the GitHub release assets):

- **Count and integrity:** `./scripts/validate-distribution.sh` expects
  exactly 30 files — 10 ZIP (8 Linux matrix entries + 2 macOS darwin), 10
  `.zip.sha256`, 10 `.sbom.json` — with checksums and CycloneDX 1.5 SBOMs
  validated, and version coherence up to the git tag.
- **Attestations:** for each ZIP asset,
  `gh attestation verify <zip> -R Goopil/php-rabbit-rs` must pass (the
  release pipeline produces a provenance and an SBOM attestation per asset;
  without `--predicate-type` both are checked). The release workflow's
  `verify-assets` job does exactly this; cite its run instead of re-running
  when possible.
- **PIE install path:** the `verify-pie-install` job installs the pre-packaged
  binaries through PIE 1.5.x (including an upgrade/rollback pass) and runs
  `amqp-smoke.sh` per cell — the functional proof that each shipped archive
  actually loads and works.

## Final decision

- [ ] All 12 criteria have evidence links and today's date, all green (or
      explicitly disclosed deviations approved in the release issue).
- [ ] RC window satisfied: ≥7 consecutive green nightly soaks against the
      RC tag.
- [ ] Tag promoted: `vX.Y.Z` (final) cut from the RC commit, release assets
      re-attested by the release pipeline.

Verdict: GO / NO-GO — decided by <name>, <date>.
