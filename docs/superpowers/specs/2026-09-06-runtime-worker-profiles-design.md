# Design — Runtime worker profile registration (`auto_subscribe`, natively)

- **Date:** 2026-09-06
- **Status:** approved (design reviewed section-by-section with owner); awaiting implementation plan
- **Source:** #164 audit item 2 (coordinator-snapshot 404 on `__auto__.` queues) + market review (vyuldashev/laravel-queue-rabbitmq 2.1k★ — declare-on-use, hosts-only minimal config)
- **Absorbs:** #167
- **Aligns with:** #164 (tracking issue), #162 (`PrefetchConfig` in `SubscriptionConfig`), #157 (typed-but-gated precedent for upstream limits)

## Motivation

The current `auto_subscribe` path has the Laravel resolver invent an
`__auto__.{queue}` profile by mutating the config array *before* pool
creation. Two failure modes follow from that design:

1. **Late resolution fails.** When the pool is created before the first pop
   (Horizon supervisor boot, Octane warm pools), the runtime queue is not in
   the built `ValidatedConfig` and `Client::consumer()` rejects it:
   `workers.{profile}: unknown worker profile` (`client.rs:331`).
2. **Even early resolution can 404.** `TopologyPlan::from_config` runs once
   at coordinator spawn (`client.rs:742`) from the immutable config. A
   queue that only exists in a runtime profile is absent from that plan, so
   the declare-before-subscribe reconcile (`recovery_coordinator.rs:703`)
   never declares it and `basic.consume` fails with 404 in `Declare` mode.

The market standard makes both moot: the reference Laravel driver works with
hosts-only config and declares queues on use. Zero-config consumption is
table stakes — dropping the auto path would make rabbit-rs-laravel the only
Laravel queue driver that requires queue pre-declaration. The gap is purely
architectural: php-amqplib declares through a synchronous channel RPC,
while this driver routes declarations through the topology plan and recovery
coordinator (pooled connections, long-lived push consumers, adaptive
prefetch).

## Goals

- `pop($queue)` works for any queue with no `workers.*` entry (market parity).
- Explicit surfaces stay first-class: `workers.*` config and a new **public**
  `Pool::registerWorker()` API.
- The 404 dies at the root: the topology plan is derived from config *plus*
  the runtime registry at reconcile time.
- Everything bounded, typed, deterministic: registry capacity bound,
  config-path-style errors, deterministic recovery order.
- No change to at-least-once semantics, the recovery-order invariant, or the
  #49 requested-vs-dormant consumer semantics.

## Non-goals

- No unregistration or "close profile" API — the registry is process-local
  and bounded; a PHP process exit reclaims everything.
- No runtime broker/exchange/vhost registration — worker profiles only.
- No persistence across a PHP process crash (same memory boundary as the
  in-flight replay buffers).
- No compatibility shim for the internal config-mutation mechanism (the
  user-visible behavior — pop just works — is unchanged).

## 1. Core: payload-carrying registry (`crates/rabbit-rs-core`)

Two structures, two roles — registration must never imply consumption:

- `Client.requested_profiles` **stays unchanged**
  (`Arc<StdMutex<HashSet<String>>>`, `client.rs:65`): request tracking, #49
  dormant semantics, and the `is_requested` gate behave exactly as today.
- **NEW** `Client.runtime_profiles: Arc<StdMutex<BTreeMap<String,
  WorkerProfile>>>` — the payload registry. It is shared into
  `CoordinatorContext` alongside `requested_profiles`, which is the only
  new plumbing in the coordinator.

- **`Client::register_worker_profile(profile: WorkerProfile) -> Result<(),
  ClientError>`** — new, called by the FFI layer, with **no** side effects
  on `requested_profiles`:
  - name present in config → identical profile is a no-op (`Ok`),
    a different payload is a typed error;
  - name present in the registry → same rule;
  - otherwise the bound check applies and the profile is inserted.
  The registry therefore holds only *additional* profiles.
- **Resolution order** in `consumer()` (`client.rs:328`): config first, then
  the runtime registry. Both are validated by the same `validate_worker`,
  so a runtime profile is indistinguishable from a config one downstream.
  The existing `requested_profiles.insert(profile)` (client.rs:338) is
  unchanged.
- **Registration timing** needs no guard: `consumer(profile)` cannot resolve
  an unregistered profile, so a profile is always registered *before* its
  first use. Mid-flight mutation cases do not exist. Registering without
  ever popping leaves the profile dormant during recovery (#49 preserved).
- **Recovery loop** (`recovery_coordinator.rs:576`) keeps its config-order
  iteration for config workers, then appends registry profiles filtered by
  the requested snapshot (name-sorted via `BTreeMap`). Deterministic,
  satisfying the recovery-order invariant.
- **`establish_requested_profile`** resolves the worker payload from
  config-or-registry instead of `context.config.worker(profile)` alone.
- **The frozen plan is deleted.** `topology_plan` is removed from
  `RecoveryCoordinatorConfig` and `CoordinatorContext` (currently
  `recovery_coordinator.rs:123`, `:164`, `:196`). The two reconcile sites
  (`:532` recovery loop, `:703` establish) compute the plan fresh:
  `TopologyPlan::from_config_and(&context.config, &runtime_extras)` where
  `runtime_extras` are registry profiles whose names are not in config.
  This is the driver's declare-on-use, and it also covers a profile
  registered after a publisher-only coordinator already spawned.
- **`TopologyPlan::from_config_and(config, extra)`** extends the existing
  `from_config` (`topology/plan.rs:268`); `from_config` remains public API
  as a thin wrapper.

## 2. FFI: `Pool::registerWorker` (`crates/rabbit-rs-php`)

- **Signature:** `Pool::registerWorker(string $name, array $profile): void`.
  The array mirrors the `workers.*` config schema (`subscriptions`,
  `scheduler`; no `name` key — the method argument supplies it). Unknown
  keys are rejected by the same `deny_unknown_fields` serde behavior as
  config.
- New converter `worker_profile_from_zval` in `conversion.rs`, then
  `validate_worker` with input path `workers.{name}` — registration errors
  are byte-identical to config-boot errors — then
  `Client::register_worker_profile`. Exceptions follow the existing Pool
  error pattern.
- **Stubs:** docblocks live in the Rust `///` docs of
  `crates/rabbit-rs-php/src/classes/pool.rs`; regenerate
  `stubs/rabbit_rs.stub.php` with `./scripts/stubs.sh --out ...` (cargo-php
  ≥ 0.1.21 needs no embed SAPI), validated by `php -l`, Pest, and the PHPT
  reflection tests.
- **`testing.rs` fake pool** gains a no-op `registerWorker` so Laravel
  Unit/Feature tests keep running extension-free.

## 3. Laravel: resolver retarget (`packages/laravel-queue`)

The `pop($queue)` auto path (shared by `RabbitMqQueue` and the Horizon
wrapper):

1. A queue mapping to an `__auto__.` profile goes through
   `WorkerProfileResolver`, which builds the profile array from queue
   options — the same shape it builds today; only the destination changes.
2. **Skip guards:** the profile exists in config workers (`hasProfile`) →
   nothing to do; the profile was already registered in this process
   (local `HashSet` on the resolver) → nothing to do. One FFI call per
   process, never per pop.
3. Otherwise `registerWorker($name, $profileArray)`, record in the local
   set, then pop exactly as before.

The pre-creation config mutation (`registerAutoProfile` writing into the
`workers` array) is removed. User-visible behavior is unchanged — pop works
without config — but it now works after pool creation too.

**TopologyMode interaction:** in `External` mode the plan compiles to no
declarations, so an auto profile's queue is never declared and
`basic.consume` surfaces the broker's 404 — the same contract as
vyuldashev's `declare => false`. Documented behavior, no code needed;
verify the exact compile path at implementation time.

## 4. Errors and bounds

| Case | Message |
|---|---|
| Duplicate differs (runtime) | `workers.{name}: profile differs from the registered profile` |
| Duplicate differs (config) | `workers.{name}: profile differs from the configured profile` |
| Registry full | `workers.{name}: profile registry is full (64 profiles)` |
| Validation | passes through `validate_worker` unchanged |
| `consumer()` miss (unchanged) | `workers.{profile}: unknown worker profile` |

- Registry bound: **64 profiles**, insertion-rejected — never evicts.
- Runtime profiles pass the same validation as config ones, so all existing
  subscription/scheduler bounds apply unchanged.
- No unregistration API (see Non-goals).

## 5. Testing (TDD — one failing test before each implementation step)

- **Core unit:** registration semantics — identical no-op, differs error,
  config collision (both branches), bound rejection, deterministic map
  iteration, and **registration alone never marks a profile requested**
  (dormant until first pop, #49).
- **Core cross-module** (`tests/`, mock transport + paused Tokio time):
  register a profile with an unseen queue → `consumer(profile)` establishes
  and *declares* it (the plan included the queue); recovery replays it;
  it stays dormant when never requested (#49); profile registered after a
  publisher-only coordinator still gets its queue declared.
- **PHP extension** (Pest, `--features extension-tests`): converter unit
  tests plus `registerWorker` happy/sad paths; PHPT reflection covers the
  new method in the stub.
- **Laravel Feature** (fake pool, no extension): `pop('__auto__.x')` calls
  `registerWorker` exactly once with the expected array shape; repeated
  pops do not re-register; a profile already in `workers.*` config never
  triggers registration.
- **Laravel Integration** (real broker): an auto queue pops and acks
  end-to-end in `Declare` mode; the poison/DLQ path stays intact.

## 6. Documentation

- `packages/laravel-queue/docs/laravel.md`: rewrite the auto_subscribe
  section — zero-config pop, `External`-mode caveat.
- `packages/laravel-queue/docs/configuration.md`: document
  `Pool::registerWorker` with a schema example.
- Root `README.md`: one-line mention of the new API.
- `CHANGELOG.md` (Unreleased): new public API entry; note the internal
  mechanism change for the auto path.
