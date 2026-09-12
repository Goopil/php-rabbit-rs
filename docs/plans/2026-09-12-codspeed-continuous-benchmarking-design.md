# CodSpeed continuous benchmarking — design (2026-09-12)

Implements #158. Amended on the same day after CodSpeed's own wizard PR
(#256, merged as 4aa427c) landed the foundation; this document records the
amended design. Supersedes the implementation sketch embedded in #158.

## Decisions

- **Foundation**: #256 (merged) ships the CI workflow, the divan harness
  (`codspeed-divan-compat`), the `bench` feature gating (custom harnesses
  break `nextest --all-targets`), and 26 sync micro-benchmarks over config,
  topology and consumer scheduling. This wave adds the remaining hot paths
  on top of that scaffolding, in the same style.
- **PHP coverage strategy**: Rust "PHP-adjacent" benches. CodSpeed has no
  native PHP support and `conversion.rs` requires a live Zend runtime, so
  PHP processing is covered by benching the pure-Rust hot path every PHP
  publish traverses (`PublishBuffer`, driven through a narrow
  `#[doc(hidden)] bench_api` module). The broker-bound driver benchmarks
  stay on the WS8 flow (#229). No walltime PHP job.
- **Gating**: blocking performance gate at >10% regression on PRs
  (dashboard configuration + required status check; manual steps below).
- **Harness**: divan (follows #256), not criterion.

## Bench inventory (final)

Landed with #256 (26 benches): config deserialize/validate/fingerprint/URI
(small + large shapes), auto-profile synthesis, `TopologyPlan::from_config`,
delay strategy/routing/buckets, attempts resolution, weighted-fair
scheduling rounds, metrics snapshot, latency percentiles.

Added by this wave (4 benches):

| Target | Crate | Measures |
|---|---|---|
| `publisher::pump_batch_128` | rabbit-rs-core | one full pipelined publish batch (128) over the mock transport: mailbox hand-off, in-flight accounting, wire write, confirmation resolution — the main hot path |
| `consumer_delivery::delivery_ack_round_trip` | rabbit-rs-core | one delivered job fetched and acknowledged over the mock transport — the per-job consumer cost |
| `consumer_delivery::delivery_ack_burst[16,64]` | rabbit-rs-core | burst fetch+ack, exercising the pipelined delivery buffer |
| `publish_buffer::publish_buffer_batch_64` | rabbit-rs-php | enqueue (conversion output) → buffer bookkeeping reads → pipelined flush → quiesce barrier: the CPU work each PHP publish performs before reaching the core |

All mock-transport benches keep real Tokio time with immediate scripted
outcomes (never pending, never timed out), so they stay CPU-bound and
deterministic under CPU simulation. The php bench links
`zend-link-stubs` (the machine-1 trick) and touches no Zend value.

## Scaffolding changes (this wave)

1. `crates/rabbit-rs-php`: `crate-type = ["cdylib", "rlib"]`; `bench`
   feature; `autobenches = false`; `[[bench]] publish_buffer`
   (`harness = false`, `required-features = ["bench"]`).
2. `#[doc(hidden)] pub mod bench_api` in the php crate re-exports
   `PublishBuffer` and `NativePublish`; the buffer's operation methods
   move from `pub(crate)` to `pub` with `# Panics` / `# Errors` docs — the
   benchmark entry surface, not part of the extension's public contract.
3. Core: two `[[bench]]` targets with the same `bench` gating.
4. CI workflow (amended): setup PHP 8.4 + libclang before `cargo codspeed
   build --workspace --features rabbit-rs-core/bench,rabbit-rs-php/bench`;
   run step runs every built suite.

## Gating

1. CodSpeed dashboard: performance gate, 10% regression threshold, applied
   to PR runs.
2. GitHub branch protection on `main`: require the
   `CodSpeed Performance Checks` status check.

No other repository workflow is modified.

## Manual steps (owner: repository admin)

- [ ] Install the CodSpeed GitHub App on `Goopil/php-rabbit-rs` (OIDC is
      primary; fallback: repository secret `CODSPEED_TOKEN`).
- [ ] Create the 10% performance gate on the dashboard.
- [ ] Add `CodSpeed Performance Checks` to the required status checks.

## Out of scope (explicit)

- PHP walltime benchmarks (no native support, higher variance).
- Driver-level broker benchmarks (WS8 owns them).
- Standalone `Metrics::record_*` benches (the methods are `pub(crate)`;
  the counters are exercised through the pump and buffer benches instead).
- Standalone `ConnectionKey::from_config` bench (hashing is already
  covered by the config benches).
- Benchmark-driven optimization work — measurement first.

## Local verification

```sh
cargo install cargo-codspeed
cargo codspeed build --workspace --features rabbit-rs-core/bench,rabbit-rs-php/bench
cargo codspeed run   # plain divan locally, nothing uploaded
```

`scripts/check.sh` is untouched: bench targets compile only via
`cargo bench` / codspeed commands with the `bench` feature, so the normal
gate (fmt + clippy + nextest + deny) never drives them.
