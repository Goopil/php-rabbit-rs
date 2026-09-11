# Rabbit RS v1 Readiness Remediation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Execution uses parallel subagents per wave; each workstream owns a strict file boundary listed in its brief.

**Goal:** Make Rabbit RS eligible for a clean, supportable 1.0 by turning every claimed-but-unproven guarantee into an enforced, observable gate.

**Architecture:** No product-scope expansion. Every workstream either (a) makes an existing claim true (Octane/mTLS certification, functional matrix, pool isolation), (b) makes an existing gate honest (RC orchestrator, doc lint, CI reliability), or (c) documents an already-decided contract. All behaviour changes are TDD; all new CI tiers land as scripts first, workflows second.

**Tech Stack:** Rust 1.96/edition 2024, Lapin behind `Transport`, PHP ≥8.4 native extension, Pest/PHPT, docker lab (`lab/rabbitmq`), Toxiproxy, GitHub Actions (digest-pinned).

**Status:** Approved 2026-09-11. Current release v0.2.2. Workstreams WS1–WS4 = Wave 1 (parallel), WS5–WS8 = Wave 2 (parallel), WS9 = Wave 3.

## Global Constraints

- No unsafe Rust. `#![forbid(unsafe_code)]` stays. Never weaken the workspace lint configuration.
- Lapin stays behind the `Transport` abstraction.
- Never expose credentials, complete AMQP URIs, or certificate material in logs, errors, metrics, or `Debug`.
- TDD for behaviour changes: focused failing test first, observe the intended failure, minimal implementation, rerun.
- Unit tests use paused Tokio time and the scriptable mock transport. No real sleeps in Rust unit tests (integration shell scripts may sleep).
- Repository artifacts (docs, commits, issues) are written in English.
- Preserve unrelated working-tree changes. Never remove or modify the untracked `scratch/` directory.
- Keep queues, channels, in-flight work, retries, and replay buffers explicitly bounded.
- Delivery is at-least-once; duplicates are permitted and measurable; silent loss after confirmed-path acceptance is a bug.
- In-memory replay is not crash-durable; docs must never claim otherwise.
- Before claiming any task complete, run its actual verification command and report the result.
- Parallel-subagent protocol: stage and commit only the files your workstream owns (explicit `git add <paths>`, never `git add -A`); on `index.lock` contention, wait and retry.

## Decision log

| # | Decision | Evidence basis |
|---|----------|----------------|
| D1 | Certify all 4 Octane servers (FrankenPHP, RoadRunner, Open Swoole, Swoole) with a real-server CI harness. PR runs 1 representative server; nightly + RC run all 4. | `ci.yml:211-235` is fake-class only; `scripts/test-octane.sh` never parses `--server=`; `packages/laravel-queue/docs/reference.md:1638-1645` claims "Certified" without evidence. |
| D2 | mTLS is supported and certified in the lab (happy path + rejected-without-client-identity). Base contract: `ca_cert` + SNI-from-host; `verify=none` stays rejected. | Core implements client identity (`config.rs:110-119`, `lapin.rs:313-379`); lab never exercises it (`fail_if_no_peer_cert=false`, no client cert). |
| D3 | Observability: documented external collection model ratified for v1. No exporter ships. Runbook + alert examples + dashboard definition. Exporter parked post-1.0. | `docs/reference.md:456,472`; `packages/laravel-queue/docs/reference.md:1419-1462`; `ROADMAP.md:833-835`. |
| D4 | TLS integration is PR-blocking (removes the `ci.yml:287` exclusion), plus nightly + RC. | `test-integration.sh --with-tls` already works locally. |
| D5 | RC window: evidence-based, minimum 1 week on the RC tag (≥7 consecutive green nightly soaks against the RC ref). | Evidence on main does not transfer to the RC tag (lockstep releases). |
| D6 | `delay.mode=auto` ≡ plugin (documented alias). TTL-fallback claims removed. No probe re-implementation. | `delay.rs:56-67` maps `Auto → Plugin`; probe deliberately removed in `de415cb`. |
| D7 | RabbitMQ floor stays 4.2.9+. Design doc corrected. | README.md:93, `docs/reference.md:19`, lab all agree; only `design.md:13,297` claims 4.3.x. |
| D8 | Distribution: NTS-only, 8 Linux + 2 macOS = 10 ZIPs (30 assets). ZTS deferred to V2. | `release/pie-matrix.json:3`, `composer.json` (`support-zts: false`). |
| D9 | Extension stays `suggest` + typed runtime error at connection resolution (`^0.2.2` at resolution). | v0.1.6 CHANGELOG decision. |
| D10 | Nightly soak kept as stabilization evidence source, after fixing the artifact-path bug. | `soak.yml:70,78` loads `.dylib` on ubuntu (Linux builds `.so`) — nightly soak fails in CI today. |
| D11 | Issue #221 fixed via direction 1 (refcounted handle claims) as a 1.0 blocker. | User decision 2026-09-11. |
| D12 | `auto_subscribe` rejected at compile time with an actionable error; dead path deleted (#164-2). | User decision 2026-09-11. |
| D13 | `TlsVerify::None` variant removed so the type is honest (#164-3). SNI override stays a documented gap (#164-4); #166 remains open post-1.0. | User decision 2026-09-11. |
| D14 | Performance: repo-stored baselines + RC budget check. CodSpeed (#158) stays open, post-1.0. | User decision 2026-09-11. |

## Evidence baseline (validated 2026-09-11)

Local gate green (fmt, clippy, 418 nextest tests, composer validate, Pint, PHPStan, cargo-deny). Confirmed gaps: TLS excluded from CI; `test-fpm.sh` never wired to CI and broker-free; Octane fake-class only; broker integration PHP 8.4-only; release assets are load-smoke only (no AMQP); no composer Dependabot/audit/SECURITY.md; `check.sh` is Rust+static-analysis only; `docs/release-checklist.md` never created (stale Task 44); `soak.yml` broken on Linux (`.dylib`); `release/` archives not gitignored; `Pool::close()` shared-handle bug (#221).

---

## Wave 1

### WS1: CI reliability — soak fix, flaky tests, gate trust

**Issues:** new `[v1] WS1: CI reliability`; absorbs #191, #189, #164-item 5.

**File ownership:** `.github/workflows/soak.yml`, `scripts/test-laravel.sh`, `packages/laravel-queue/tests/Integration/PoisonDeliveryTest.php`, the PHPT file named in #189 (`crates/rabbit-rs-php/tests/phpt/`), plus the driver-bench/soak docs if a tripwire needs documenting.

**Tasks:**

1. **Soak artifact path (D10).** Modify `.github/workflows/soak.yml:70,78`: `librabbit_rs_php.dylib` → `librabbit_rs_php.so` (workflow runs only on ubuntu). Verification: `gh workflow run soak.yml -f steady_minutes=2 -f kill_minutes=2` on the branch; before fix the "Soak — steady segment" step fails to load the extension; after fix it is green and artifacts show `missing: 0` and terminal `publish_buffered == 0`.
2. **#191 PoisonDeliveryTest timing flake.** Make the dead-letter assertion deadline-based (poll the DLQ/management API until timeout instead of fixed timing assumptions). Test-first: reproduce locally under parallel load if possible; otherwise pin determinism by polling. Acceptance: 10 consecutive local green runs of the affected test with load.
3. **#189 PHPT AsyncFlushTest flake on PHP 8.5 in Docker.** Same principle: replace fixed timing with deadline polling inside the PHPT where the harness allows; document any harness limitation. Acceptance: 10 consecutive green PHPT runs in Docker PHP 8.5.
4. **#164-item 5: 10 "PHPUnit Notices" via `scripts/test-laravel.sh`.** Diagnose the PHP-binary mismatch between script and shell default (A/B-verified in #164); make the script resolve the same binary `vendor/bin/pest` would, or pin explicitly. Acceptance: script output reports 0 notices on a pristine checkout; direct pest run and script run agree.

**CI tier:** nightly + manual (soak), PR (the rest). **Acceptance:** nightly soak green with artifacts; flaky tests pass 10× consecutively; script/pest parity. **Rollback:** revert individual fixes; never delete the tests.

### WS2: Refcounted pool claims — fix #221 (1.0 blocker)

**Issues:** #221 (direction 1, D11).

**File ownership:** `crates/rabbit-rs-core/src/pool/mod.rs`, `crates/rabbit-rs-core/src/client.rs`, `crates/rabbit-rs-core/src/runtime.rs` (only if the registry needs it), `crates/rabbit-rs-php/src/classes/pool.rs`, plus tests in `crates/rabbit-rs-core/tests/` and `crates/rabbit-rs-php/tests/`, and `crates/rabbit-rs-php/tests/Feature/RouteBindingTest`-equivalent (unpin if the Laravel-side pin exists — check `packages/laravel-queue/tests/` for the pool-free pin comment).

**Mechanism (from #221):** `Pool::__construct` acquires a shared `ConnectionHandle` from the process-local `RuntimeRegistry` keyed by `ConnectionKey::from_config`; `Pool::close()` flips the handle's `closed` AtomicBool with no use count, killing every sibling pool with the same fingerprint.

**Tasks:**

1. **Test first (core, mock transport):** two pools, same fingerprint → `close()` pool A → pool B operations keep working (publish/consume/stats); `close()` pool B (last claim) → the shared connection is actually torn down (handle closed, registry can retire it). Paused-time unit test. Expected failure before the fix: pool B throws `cannot use a closed pool`.
2. **Test first (ext, Pest):** open live pool + transient probe pool (same native config, doctor pattern) → close the probe → live pool still functional (`stats()`, publish). Expected failure before: `cannot use a closed pool`.
3. **Implement:** per-claim refcount on the shared `ConnectionHandle`; `close()` releases the caller's claim; the connection closes when the last claim drops; registry replacement only for handles with zero live claims. Bounded: no unbounded claim growth (claims live only while a PHP `Pool` object exists).
4. **Unpin** the Laravel-side test that was made pool-free because of this bug (search for the pin comment referencing probe/doctor races).
5. Update `docs/reference.md` reliability section if it documents current close semantics.

**CI tier:** PR. **Acceptance:** new core + ext tests green; full `rtk cargo test -p rabbit-rs-core` green; Pest suite green; FPM isolation script still green (`./scripts/test-fpm.sh`). **Rollback:** revert the refcount commit; #221 stays open.

### WS3: TLS mandatory PR gate + mTLS certification + honest TLS surface

**Issues:** new `[v1] WS3: TLS`; links #166 (post-1.0); absorbs #164-items 3 and 4 (D2, D4, D13).

**File ownership:** `lab/rabbitmq/tls/generate.sh`, `lab/rabbitmq/compose.yaml`, `lab/rabbitmq/rabbitmq/rabbitmq-mtls.conf` (new), `lab/rabbitmq/rabbitmq/rabbitmq-tls.conf` (if needed), `crates/rabbit-rs-core/tests/tls_integration.rs`, `crates/rabbit-rs-core/src/config.rs`, `crates/rabbit-rs-core/src/transport/lapin.rs`, `crates/rabbit-rs-core/src/config.rs` tests, `.github/workflows/ci.yml` (TLS exclusion removal — the ONLY wave-1 agent allowed to touch `ci.yml`), `CHANGELOG.md`.

**Tasks:**

1. **Remove `TlsVerify::None` (D13).** Test first: `verify: none` in config YAML fails deserialization with a typed `unknown variant` error; the existing test `tls_verify_none_is_rejected_at_validation` (`config.rs`) is replaced accordingly. Then remove the variant from `TlsVerify` (`config.rs:82-97`), its validation branch (`config.rs:568-578`), and the transport re-check (`lapin.rs:324-327`). CHANGELOG entry (intentional pre-1.0 break).
2. **mTLS lab (D2).** Extend `lab/rabbitmq/tls/generate.sh`: `lab-client.pem` + `lab-client-key.pem` signed by the lab CA (`chmod 600`). Add a `rabbitmq-mtls` service to the `with-tls` profile in `compose.yaml` with its own config (`ssl_options.fail_if_no_peer_cert = true`, listener 5673; per-node options are global, hence a second node). Mount certs read-only.
3. **mTLS + SAN tests.** Add to `tls_integration.rs`:
   - `mtls_handshake_succeeds_with_client_identity` (127.0.0.1:5673 with CA + client cert/key) — expected failure before: no client cert exists / listener absent.
   - `mtls_handshake_fails_without_client_identity` (typed transport error).
   - `tls_handshake_fails_when_host_is_not_in_the_certificate_san` (connect with host `wrong.internal` mapped to 127.0.0.1 via /etc/hosts in the CI step; cert SANs do not include it).
4. **CI (D4).** In `.github/workflows/ci.yml` integration job: remove `-E 'not binary(tls_integration)'` and run the TLS suite (either via `./scripts/test-integration.sh --with-tls` or by adding the TLS lab profile + tests to the existing job).
5. **Hygiene check:** `git check-ignore -v lab/rabbitmq/tls/generated/lab-client-key.pem` matches the existing `lab/rabbitmq/tls/.gitignore` rule; confirm no key material can be committed.
6. **Docs:** `docs/reference.md` TLS section: mTLS supported (certified by tests), SNI always equals the connection host (documented gap, #166 post-1.0), `verify=none` removed.

**CI tier:** PR (blocking), nightly, RC. **Acceptance:** 6 TLS cases green in CI (trusted CA, untrusted CA, host-in-SAN, host-not-in-SAN, mTLS success, mTLS reject); no key material committed; `cargo test -p rabbit-rs-core config::tests` green. **Rollback:** re-add the `-E` exclusion; keep the mTLS lab work staged on the branch.

### WS4: Security/DX hygiene

**Issues:** new `[v1] WS4: security/DX hygiene`.

**File ownership:** `.github/dependabot.yml`, `SECURITY.md` (new), `.gitignore`, `README.md` (only the security-policy link line). **MUST NOT touch `ci.yml`** (WS3 owns it in wave 1; the composer-audit job lands in wave 2 after WS3 merges).

**Tasks:**

1. **Dependabot composer (C1).** Add `composer` ecosystems (weekly, like existing entries) for `packages/laravel-queue`, `crates/rabbit-rs-php`, `benchmarks`, `benchmarks/driver-bench`.
2. **SECURITY.md (C2).** Root file: private vulnerability reporting (GitHub Security Advisories), supported versions (pre-1.0: latest 0.x only; post-1.0: latest 1.x), response expectations (ack ≤ 3 business days, fix ≤ 90 days, critical expedited), credential-reporting guidance (never paste full AMQP URIs or credentials; use redacted `rabbit-rs:doctor` output; redaction contract per `docs/reference.md:493-498`). Link from README.
3. **Release-asset hygiene (C3).** `.gitignore`: add `release/*.zip`, `release/*.sha256`, `release/*.sbom.json` (keep `release/pie-matrix.json` tracked). Verify: `git check-ignore -v release/foo.zip` matches; `release/pie-matrix.json` still tracked.

**CI tier:** PR (job wiring lands wave 2). **Acceptance:** dependabot config valid YAML with 4 new composer entries; SECURITY.md present + linked; gitignore verified by `git check-ignore`. **Rollback:** revert individual files.

---

## Wave 2

### WS5: Runtime certification — real Octane servers + broker-backed FPM

**Issues:** new `[v1] WS5: runtime certification` (D1).

**File ownership:** `scripts/test-octane-runtime.sh` (new), `scripts/test-fpm.sh`, `packages/laravel-queue/tests/Runtime/` (new), `crates/rabbit-rs-php/tests/fixtures/fpm/` (new publish/consume fixtures), `.github/workflows/nightly.yml` (new), `.github/workflows/ci.yml` (new octane-runtime + fpm jobs — wave 2, no conflict), `packages/laravel-queue/docs/reference.md` (certification table).

**Tasks:**

1. **Octane harness.** `scripts/test-octane-runtime.sh --server=<name>`: builds ext, `./scripts/lab-up.sh with-plugin`, per-server setup (RoadRunner: pinned `rr` binary; FrankenPHP: `dunglas/frankenphp` docker image; Swoole/Open Swoole: `shivammathur/setup-php` extensions), starts `octane:start`, drives the scenario app (`packages/laravel-queue/tests/Runtime/`): `POST /publish` (one `Queue::push`, no follow-up op — the #218 pattern), `GET /consume-one`, `GET /stats`; scenario assertions: after publish ×5 → `octane:reload` → publish ×5 → graceful stop: management-API queue depth == 10 (no loss), `publish_buffered == 0` (graceful-stop flush), consume 10 → ack all → depth 0. PR job runs roadrunner; nightly runs all four.
2. **FPM broker-backed scenarios.** Extend `scripts/test-fpm.sh` with lab wiring (docker check + `lab-up.sh with-plugin` + trap, mirroring `test-integration.sh:25-33`): scenario B — request runs new fixture `publish.php` (lone publish, no follow-up), idle ≥ `flush_interval`, management-API depth == 1 (pins #218 under real FPM); scenario C — publish then immediate `SIGTERM` php-fpm → depth == 1 within deadline; scenario D — `SIGUSR2` reload, re-run the existing 2-worker isolation assertions (L162-185 unchanged).
3. **CI jobs** (wave 2): `octane-runtime` (roadrunner, PR-blocking) + `fpm` (docker `php:${{ matrix.php }}-fpm` with extension bind-mounted, pattern from the `phpt` job `ci.yml:193`).

**CI tier:** PR (roadrunner + FPM), nightly + RC (all four servers). **Acceptance:** all four servers green nightly; FPM scenarios green; docs table cites run evidence. **Rollback:** drop a failing server from the v1 contract per the brief (doc change + decision-log update), never silently.

### WS6: Functional compatibility matrix

**Issues:** new `[v1] WS6: functional matrix`.

**File ownership:** `scripts/amqp-smoke.sh` (new), `.github/workflows/nightly.yml` (shared with WS5 — coordinate: WS5 creates it in wave 2; WS6 appends matrix jobs), `.github/workflows/release.yml` (smoke step in `verify-pie-install` cells), `docs/reference.md` (matrix table).

**Tasks:**

1. **`scripts/amqp-smoke.sh`:** parameterized by PHP binary + extension artifact: load, publish(5)→confirms ack, consume+ack, one recovery scenario (Toxiproxy pause 5 s on a per-test proxy → republish succeeds), non-zero exit on any loss.
2. **Nightly workflow:** `schedule` + `workflow_dispatch` + `workflow_call`; jobs: integration PHP 8.5 (glibc x86_64), musl x86_64, glibc arm64, musl arm64 (docker `php:*-alpine`/arm images running `amqp-smoke.sh` against the runner-hosted lab via `--add-host=host.docker.internal:host-gateway`), plus WS5's octane-runtime matrix.
3. **Release smoke:** one `amqp-smoke.sh` step per `verify-pie-install` cell (shared lab brought up once).
4. **Docs:** `docs/reference.md` distribution table marking each cell functional (evidence) or build-only; no silent build-only cells.

**CI tier:** nightly + RC (PR unchanged). **Acceptance:** nightly green across cells; release cells each prove publish/consume/confirms/recovery. **Rollback:** shrink nightly matrix; mark remaining cells build-only explicitly.

### WS7: Contract reconciliation — docs coherence + auto_subscribe

**Issues:** new `[v1] WS7: contract reconciliation`; absorbs #164-item 2 (D6–D9, D12).

**File ownership:** `scripts/check-docs.sh` (new), `docs/plans/2026-07-30-rabbitmq-native-design.md`, `docs/plans/2026-07-30-rabbitmq-native-implementation.md`, `docs/reference.md`, `packages/laravel-queue/docs/reference.md`, `crates/rabbit-rs-core/src/config.rs` (docblock + test name only), `README.md`, `packages/laravel-queue/src/Config/ConnectionCompiler.php`, `packages/laravel-queue/src/Support/WorkerProfileResolver.php` (delete `registerAutoProfile` dead path), `packages/laravel-queue/config/rabbit-rs.php` (remove/comment `auto_subscribe`), `packages/laravel-queue/tests/`, `.github/workflows/ci.yml` (docs job — wave 2).

**Tasks:**

1. **`auto_subscribe` rejection (D12).** Test first (Pest Unit): compiling a connection config with `auto_subscribe: true` throws an actionable `InvalidArgumentException` naming the option and the reason. Expected failure before: it passes through and produces the confusing native error later. Then reject in `ConnectionCompiler`, delete `registerAutoProfile` and the implicit `__auto__` path, update package docs + CHANGELOG.
2. **Stale docblock (post-#218).** `config.rs:461-483` docblock → background-timer contract (per `crates/rabbit-rs-php/src/classes/publish_buffer.rs:14-18`); rename `config.rs:2331` test to reflect timer semantics.
3. **Doc lint.** Create `scripts/check-docs.sh`: fails on stale claims outside `CHANGELOG.md`/`docs/plans/ROADMAP.md`/historical sections: "16 release archives", "RabbitMQ 4.3", "NTS and ZTS", "plugin when available", "TTL buckets otherwise", require-model `ext-rabbit_rs` claims in `docs/reference.md`. Plus a cross-check that every metric name in `docs/operations/alerts.md` (WS9 deliverable; tolerant if absent) exists in `crates/rabbit-rs-core/src/metrics.rs`.
4. **Doc fixes (D6–D9):** design doc (`design.md:13,151-156,297,309-323,338,342,349`): 4.2.9+, auto≡plugin, NTS-only/30 assets, suggest model, runtime certification = evidenced release checklist, observability external-collection model. Implementation plan header + 16-combination text (`:30-34,2394-2400,2609`). `docs/reference.md:160,207` suggest model + `^0.2.x` constraint. `packages/laravel-queue/docs/reference.md:1721` recipe row. README support-contract table (PHP, SAPIs, Octane, RabbitMQ floor, NTS, ext constraint, delivery semantics).
5. **CI:** add a `docs` job running `check-docs.sh` (wave 2 ci.yml edit).

**CI tier:** PR. **Acceptance:** lint green; contract stated identically across README/design/reference/package docs; `rtk cargo test -p rabbit-rs-core config::tests` green. **Rollback:** narrow the lint phrase list (never delete the lint).

### WS8: Performance baselines + RC budget check

**Issues:** new `[v1] WS8: perf baselines`; links #158 (post-1.0, D14).

**File ownership:** `benchmarks/baselines/reference-machine.json` (new), `benchmarks/baselines/check-budgets.php` (new), `docs/performance.md` (new), `benchmarks/README.md`, `.github/workflows/release-candidate.yml` (WS9 may own the scaffold — coordinate).

**Tasks:**

1. **Budget checker.** `check-budgets.php`: compares a result JSON against baseline + thresholds (throughput ≥ 80 % of baseline, p99 ≤ 150 %, `missing == 0` and `duplicates == 0` always blocking). Test first: run against `benchmarks/results/round-k-soak/*.json` with a deliberately too-low threshold → expected FAIL; correct threshold → PASS.
2. **Baseline:** generate `reference-machine.json` once via `scripts/rebench-driver-bench.sh` on the documented runner spec (machine, PHP, driver versions recorded in `docs/performance.md`).
3. **Docs + wiring:** `benchmarks/README.md:77` — replace "no CI runs the runner / informational" with: nightly optional, RC mandatory comparison. RC workflow runs rebench + budget check.

**CI tier:** RC (blocking), nightly optional. **Acceptance:** baseline stored; RC comparison fails on >20 % throughput regression or any loss. **Rollback:** thresholds live in the JSON; relax without code change, document the relaxation.

---

## Wave 3

### WS9: RC pipeline & go/no-go (issue #177)

**File ownership:** `scripts/verify-release-candidate.sh` (new), `.github/workflows/release-candidate.yml` (new), `docs/release-checklist.md` (new — completes stale implementation-plan Task 44), `docs/operations/runbook.md` + `alerts.md` + `dashboard.json` (D3 deliverables; may be a separate PR), `docs/development.md`.

**Tasks:**

1. **Observability pack (D3):** `docs/operations/runbook.md` (incident classes: broker down, backpressure growth, dropped publications ≠ 0, stuck publish buffer, duplicates spike, poison terminal settle — symptom → signal → action), `alerts.md` (rules referencing only existing metrics: `reconnects_total`, `backpressure_total`, `dropped_publications_total`, `publication_retries_total`, `duplicates_total`, queue depth; caveat: management-API `redelivered` is an approximate cross-process duplicate signal), `dashboard.json` (Grafana over the documented sidecar exporter + management API).
2. **RC orchestrator:** `scripts/verify-release-candidate.sh` with `--dry-run` (validates prerequisites, prints tier plan): `check.sh` → `test-extension.sh` → `test-laravel.sh` → `test-integration.sh --with-tls` → `test-fpm.sh` → `test-octane-runtime.sh` ×4 → `validate-distribution.sh` (with `release/` archives when present) → `amqp-smoke.sh` per present artifact → budget check (WS8). Blocking = everything except soak evidence.
3. **RC workflow:** tags `v*-*rc*` + `workflow_dispatch`; calls `nightly.yml` (workflow_call) + runs the orchestrator + uploads evidence.
4. **`docs/release-checklist.md`:** the go/no-go checklist below with a fill-in evidence table (versions, soak links, cert matrix, asset + attestation verification).
5. **`docs/development.md`:** prerequisites, expected duration (~2–3 h full RC pass), blocking vs advisory tiers.
6. **Composer-audit CI job** (deferred from WS4 to avoid wave-1 `ci.yml` conflict): `composer audit` in `packages/laravel-queue` + `crates/rabbit-rs-php`.

**CI tier:** RC tags (blocking) + manual. **Acceptance:** dry-run green; a real RC tag run completes with archived evidence. **Rollback:** any tier can be marked advisory in the script's tier table with an explicit docs note.

## Go/No-Go checklist for 1.0 (observable criteria only)

1. `./scripts/check.sh` exits 0 on the RC commit.
2. PR-tier CI green on the RC commit, including: TLS suite (6 cases), broker-backed FPM, one Octane runtime server, doc lint, composer audit.
3. `./scripts/verify-release-candidate.sh` exits 0 on the RC tag; evidence archived as workflow artifacts.
4. Nightly evidence against the RC ref: ≥7 consecutive green soaks, each showing `missing == 0`, terminal `publish_buffered == 0`, leak slope within documented budget, 100 % reconnect recovery.
5. Octane certification: green evidence recorded for FrankenPHP, RoadRunner, Open Swoole, Swoole (publish → reload → graceful stop → no loss → `publish_buffered == 0`).
6. FPM certification: green evidence for 2-worker isolation, lone-publish age-flush, graceful-stop flush, reload survival.
7. Functional matrix table: every cell (PHP 8.4/8.5 × glibc/musl × x86_64/ARM64, macOS ARM64) green or explicitly marked build-only.
8. Distribution: `validate-distribution.sh` green with 30 assets; `gh attestation verify` green per asset; `verify-pie-install` + upgrade/rollback green; each install cell passes `amqp-smoke.sh`.
9. `docs/release-checklist.md` complete: versions, dates, evidence links, benchmark comparison within thresholds.
10. Contract coherence: `scripts/check-docs.sh` green; README support table matches `release/pie-matrix.json`, `composer.json`, design doc.
11. Security: `SECURITY.md` live; Dependabot composer active; `composer audit` + `cargo deny check` green.
12. #221 fixed via refcounted claims; the Laravel-side pin removed; sibling-pool isolation tests green.

## Independence map

WS1, WS2, WS3, WS4 mutually independent (wave 1; WS3 exclusively owns `ci.yml` in wave 1, WS4 is forbidden from it). WS5, WS6, WS7, WS8 mutually independent (wave 2; WS5 and WS6 coordinate on `nightly.yml` — WS5 creates, WS6 appends). WS9 depends on WS1–WS8 deliverables. WS2 is the only code-correctness blocker; WS1 unblocks evidence trust (flaky gates poison the ≥7-nightly criterion).
