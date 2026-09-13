# Consume Path Attribution: Lab Instrument + Hop Benchmarks

Date: 2026-09-13
Issue: [#282](https://github.com/Goopil/php-rabbit-rs/issues/282)
Status: approved design

## Context

#282 established that the ~3x consume gap between rabbit-rs (~30k msg/s) and
bunny (~90k msg/s) is supply-side wake-chain latency, not the pop path. Every
thread is ~80% parked: each delivery crosses ~2-4 sequential cross-thread/task
wakeups (lapin-io → `spawn_source` → actor → flume → PHP condvar), costing an
estimated 26-34 µs per message.

Lead 5 in #282 ("measure lapin's share first") gates leads 1-3. Today no
measurement isolates the Rust pipeline from PHP: the PHP benchmarks mix
extension, wake chain, and driver costs; the existing current-thread bench
(`consumer_delivery.rs`) measures code-path cost without cross-thread wakes or
lapin. This design adds the missing measurement instruments. It changes no
pipeline behavior.

## Goal

Produce a firm attribution table for the consume path:

```
current-thread bench (code cost)  ─┐
lab instrument Rust-only           ├─ deltas split code / wakes / lapin / PHP
lab instrument + `sample` profile  │
PHP benchmark (#283 semantics)    ─┘
```

and re-rank optimization leads 1-3 from data instead of intuition.

## Non-Goals

- No pipeline code changes (leads 1-4 stay unimplemented until ranked).
- No multi-thread bench in CodSpeed: scheduler-dependent wake costs are
  explicitly excluded from tracked benches (see the runtime comment in
  `consumer_delivery.rs`).
- No new profiling framework; macOS `sample` is sufficient.
- No criterion: divan is the repo standard and what CodSpeed tracks.

## Instrument 1: `crates/rabbit-rs-core/examples/lab_consume.rs`

A minimal binary (~150 lines, manual arg parsing, no new dependencies) that
measures the Rust-only consume ceiling against the real lab broker:

- Connects through the public `ClientPool` to `rabbit.local:5672`
  (guest/secret — same defaults as the integration tests), overridable via
  `LAB_AMQP_URI`.
- Declares an external-topology queue `lab-consume-bench`, purged before the
  run; publishes N messages first (fire-and-forget confirms), waits for the
  drain, then measures the consume phase only.
- Args: `--messages N` (default 100_000), `--seconds T` (default 30),
  `--no-ack` to switch acknowledgement semantics. The run stops at whichever
  limit is reached first; both limits apply simultaneously.
- Output: one JSON-ish stats line (msg/s, p50, p99 per message) suitable for
  pasting into #282.
- Not part of CodSpeed or CI; run manually with the lab up.

## Instrument 2: `crates/rabbit-rs-core/benches/consumer_hops.rs`

Divan benchmarks (tracked by CodSpeed) isolating one hop each, on a
current-thread runtime with the mock transport — same discipline as
`consumer_delivery.rs`:

| Bench | Measures | Decides |
|---|---|---|
| `mpsc_hop` | `commands.send(Incoming)` + receiver wake, per message | Lead 1 (batch the lapin→actor hop) |
| `flume_hop` | flume send + recv by a blocked receiver, per message | Cost of the actor→pop channel itself |
| `notify_per_pop` | `dispatch_notify.notify_one()` + actor wake, per pop | Lead 2 (dispatch-notify watermark) |
| `actor_drain` | N ready commands: one select per iteration vs draining all ready (`args = ["per_command", "drain"]`) | Lead 3 (actor loop drain) |

Details:

- Reuses `benches/support.rs` and tokio/flume primitives directly; no extra
  mocks.
- `actor_drain` measures the two structures as such (pre-filled command
  queue); it is a structural bound, not a before/after of current code.
- Realistic 38-byte payload (`Bytes::from_static`), single subscription,
  English doc comments, existing bench style.

## Measurement Protocol

1. `./scripts/lab-up.sh`
2. `cargo run -p rabbit-rs-core --example lab_consume -- --messages 100000`
   (repeat with `--no-ack`)
3. During the run: `sample <pid> 4 -file /tmp/lab-consume.txt`
4. `cargo bench -p rabbit-rs-core --features bench -- consumer_hops`
5. Compare against the PHP numbers already recorded in #283 semantics
   (30.5k rabbit-rs / 85k bunny).

## Deliverables

- One PR with the example and the bench file (one MR per delivery, pack
  convention).
- A comment on #282 with the attribution table (hop → µs/msg → share of the
  ~30 µs budget), the re-ranked leads 1-3, and follow-up issue(s) for the
  top-ranked lead.

## Guardrails

- At-least-once semantics untouched; no production code modified.
- The example never runs in CI; benches stay deterministic (current-thread,
  mock transport).
- Results recorded in #282 keep the documented-numbers history in one place.
