# CodSpeed continuous benchmarking — design (2026-09-12)

Implements #158 with the scope agreed in design review. Supersedes the
implementation sketch embedded in #158 where it differs (bench target list,
PHP-adjacent coverage, gating decision).

## Decisions

- **PHP coverage strategy**: Rust "PHP-adjacent" benches. CodSpeed has no
  native PHP support and `conversion.rs` requires a live Zend runtime, so
  PHP processing is covered by benching the pure-Rust hot path every PHP
  publish/consume traverses (`PublishBuffer`) plus the core hot paths. The
  broker-bound driver benchmarks stay on the WS8 flow (#229: stored
  baselines + budget checker). No walltime PHP job.
- **Gating**: blocking performance gate at >10% regression on PRs
  (configured on the CodSpeed dashboard + required status check).
- **CI**: one workflow, simulation mode, OIDC authentication.

## Bench targets (7, broker-free, criterion harness)

| Target | Crate | Measures |
|---|---|---|
| `config` | rabbit-rs-core | `Config::validate()` end-to-end (happy paths + typed error paths) |
| `pool_key` | rabbit-rs-core | connection-key derivation from config |
| `publisher_pump` | rabbit-rs-core | pipelined publish loop over `MockTransport` — the main hot path |
| `consumer_delivery` | rabbit-rs-core | delivery → ack/reject round-trip over `MockTransport` |
| `metrics` | rabbit-rs-core | metrics recording on publish/consume paths |
| `topology_declare` | rabbit-rs-core | config → topology declaration plan construction |
| `publish_buffer` | rabbit-rs-php | enqueue → flush (threshold/age) → pop → teardown cycles over `MockTransport`; the CPU work each PHP publish performs before reaching the core |

All benches are broker-free: the three transport-driven benches
(`publisher_pump`, `consumer_delivery`, `publish_buffer`) declare
`required-features = ["test-support"]` and run against the scriptable mock
transport with real Tokio time (no paused clocks in benches) and healthy
deadlines.

## Scaffolding changes

1. Both crates gain the dev-dependency
   `criterion = { package = "codspeed-criterion-compat", version = "5" }`
   (passthrough over criterion 0.5: plain `cargo bench` behaves like stock
   criterion).
2. `crates/rabbit-rs-php`: `crate-type = ["cdylib", "rlib"]` so bench
   targets can link the crate; `PublishBuffer` and its operation methods
   move from `pub(crate)` to `pub` (the bench entry surface; documented as
   the benchmark/test API).
3. `[[bench]]` sections with `harness = false` for each target.
4. Bench hygiene: `black_box` on every consumed value, `measurement_time`
   bounded (~3 s per bench, full run < 1 min), throughput reporting on the
   pump/buffer benches.

## CI workflow (`.github/workflows/codspeed.yml`)

- Triggers: `push` on `main`, all `pull_request`s, `workflow_dispatch`
  (backtest analysis).
- Job `benchmarks` on `ubuntu-latest`, permissions
  `contents: read`, `id-token: write` (OIDC auth with CodSpeed).
- Steps: checkout (pinned SHA) → `./.github/actions/rust-setup`
  (channel 1.96, release cache) with `cargo-codspeed` installed →
  PHP 8.4 via `shivammathur/setup-php` + `libclang-dev` (needed to compile
  the `publish_buffer` bench against ext-php-rs) → `cargo codspeed build`
  (with the feature flags required by the mock-transport benches; verify
  at implementation whether `cargo codspeed build` forwards `--features`,
  otherwise build per-package) → `CodSpeedHQ/action@v5`, mode
  `simulation`, run `cargo codspeed run`.

## Gating

1. CodSpeed dashboard: performance gate, 10% regression threshold, applied
   to PR runs.
2. GitHub branch protection on `main`: require the
   `CodSpeed Performance Checks` status check.

No other repository workflow is modified.

## Manual steps (owner: repository admin)

- [ ] Install the CodSpeed GitHub App on `Goopil/php-rabbit-rs`
      (app.codspeed.io → import repository). OIDC is the primary auth;
      fallback: repository secret `CODSPEED_TOKEN` referenced by the
      workflow.
- [ ] Create the 10% performance gate on the dashboard.
- [ ] Add `CodSpeed Performance Checks` to the required status checks.

## Out of scope (explicit)

- PHP walltime benchmarks (no native support, higher variance; revisit if
  the PHP-adjacent benches prove insufficient).
- Driver-level broker benchmarks (WS8 owns them).
- Benchmark-driven optimization work — this PR only establishes
  measurement.

## Local verification

```sh
cargo install cargo-codspeed
cargo codspeed build
cargo codspeed run   # plain criterion locally, nothing uploaded
```

`scripts/check.sh` is untouched: bench targets compile only via
`cargo bench` / codspeed commands, so the normal gate is unaffected.
