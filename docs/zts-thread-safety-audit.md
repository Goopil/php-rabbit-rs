# ZTS Thread-Safety Audit — Pre-Implementation Gate for Issue #369

Status: audit complete (read-only). Scope: whether *shipping* ZTS prebuilt
binaries (`support-zts: true` + ZTS release assets) is safe, given that the
nightly harness already builds and certifies an in-image ZTS extension.

Audited code: `crates/rabbit-rs-core` (registry, runtime, metrics) and
`crates/rabbit-rs-php` (`callbacks.rs`, `bridge.rs`, `lib.rs`, plus the PHP
class boundary the bridge serves). Dependencies inspected at their pinned
versions: ext-php-rs 0.16.3 (`crates/rabbit-rs-php/Cargo.toml:25`), tokio
1.54.1. No code was modified.

## Executive verdict

**ZTS is NOT safe to ship as-is.** The Rust core is memory-safe under
concurrent PHP threads (everything shared is `Mutex`/atomic protected, and Zend
values never cross threads), but the module's **MSHUTDOWN handler closes the
process-global runtime registry**, which under ZTS runs on *every* worker
thread teardown. The first recycled thread therefore kills the shared Tokio
runtime and all pooled connections underneath the surviving threads; tokio's
documented behavior for `Handle::block_on` against a shut-down runtime is that
timers **panic** (tokio 1.54.1 `src/runtime/handle.rs:257-262, 322-323`). The
nightly certification never sees this because it runs `--workers=1`
(`scripts/test-octane-runtime.sh:432`).

Required fixes before flipping `support-zts` — all minimal:

1. **`crates/rabbit-rs-php/src/lib.rs:30-33`** — `module_shutdown` must not
   close `RuntimeRegistry::global()` on ZTS builds. Gate the `close()` on
   `!ext_php_rs::PHP_ZTS` (the const exists: ext-php-rs 0.16.3
   `src/lib.rs:92`, driven by the same `cfg(php_zts)` that selects the ZTS
   module entry in `src/builders/module.rs:814-831`, proven correctly set by
   the in-image build). NTS behavior is unchanged. Consequence under ZTS: a
   worker restart no longer severs the shared runtime; the registry is
   reclaimed at process exit, which is exactly when MSHUTDOWN close has value.
2. **Harness, before the first ZTS release (`scripts/test-octane-runtime.sh:432`)**
   — the frankenphp certification must run with `--workers>1` (2–4) plus
   max-requests recycling, so true concurrent multi-thread load and the
   cross-thread MSHUTDOWN timing are exercised. The single-worker green run
   cannot certify the configuration users actually run.
3. **Docs shipped with the release** — `docs/reference.md:574` claims the
   registry is shared across PHP threads "without synchronization". That
   rationale is outdated (synchronization exists; the real gap is lifecycle and
   ownership isolation — see findings F1/F3). Correct it and document the
   shared-`ClientPool`/shared-`ConsumerHandle` semantics under ZTS
   (`crates/rabbit-rs-core/src/client.rs:419-431`,
   `crates/rabbit-rs-core/src/consumer/composite.rs:70-76`).

With 1–3 applied, ZTS binaries are shippable. F1 is the only code change.

---

## Q1 — RuntimeRegistry global state under concurrent PHP worker threads

**Structure.** `RuntimeRegistry::global()` is a process-wide `OnceLock`
(`crates/rabbit-rs-core/src/runtime.rs:104-108`); the mutable process state is
`Mutex<Option<ProcessState>>` (`runtime.rs:85-90`), holding the PID, the Tokio
runtime, and the pool map (`runtime.rs:78-82`).

**Concurrent `acquire`.** `acquire()` reads the PID and performs inherited-state
invalidation and lazy runtime creation under the mutex
(`runtime.rs:149-178`). Pool reuse/insert and the claim increment also happen
under the same lock (`runtime.rs:180-200`, `add_claim` at :185 and :190), so a
pool created by thread A always carries a live claim before thread B can
observe it — `take_oldest_idle_pool` skips pools with live claims
(`runtime.rs:277-289`), so concurrent construction cannot evict a pool another
thread just acquired. Eviction teardown deliberately runs *outside* the lock
(`runtime.rs:208-215`, `close_evicted_pool` :298-317) at the cost of blocking
the evicting PHP thread for up to the shutdown budget — latency, not unsafety.

**Shared runtime and client.** The runtime factory is a 1-worker multi-thread
Tokio runtime (`runtime.rs:50-58, 60-69`). `ConnectionHandle` state is
atomics + `OnceLock` (`src/pool/mod.rs:31-37`); claim accounting is atomic
(`pool/mod.rs:65-91`). Critically, `ConnectionHandle::client()` attaches one
`ClientPool` per config fingerprint via `OnceLock::get_or_init`
(`pool/mod.rs:124-128`), and `Pool::__construct` fetches its client that way
(`crates/rabbit-rs-php/src/classes/pool.rs:84-87`). Under ZTS, N worker
threads therefore share **one** `ClientPool` (publishers, consumers, metrics)
and **one** Tokio runtime.

**Concurrent calls into the shared client.** `ClientPool` guards every map
(lifecycle, coordinators, publishers, consumers, initializers) with
`StdMutex` (`src/client.rs:113-131`); actors are channel-driven tasks on the
shared runtime. Concurrent `publish_batch` from two PHP threads is
memory-safe and semantically the same interleaving the runtime already
produces. `Metrics` is a lock-free atomic registry (`src/metrics.rs:25-29,
135-156`), safe to record from any actor or PHP thread; snapshots are
best-effort non-atomic views by contract (`metrics.rs:32-35`).

**Concurrent `Handle::block_on`.** PHP-side operations block on
`handle.runtime().block_on(...)` from the calling PHP thread
(`pool.rs:218, 254-257, 273-277, 628-635` etc.). Multiple foreign threads may
block concurrently on a multi-thread scheduler; the futures are polled on the
calling threads, actors on the runtime worker. Supported by tokio — **except
when the runtime has been shut down**: see F1.

**Consumer sharing semantics.** `ClientPool::consumer()` returns a cached
`ConsumerHandle` per profile when fresh (`client.rs:419-431`), and
`ConsumerHandle` is a cloneable `Arc<CompositeInner>`
(`src/consumer/composite.rs:58-61, 96-104`). Under ZTS, two requests popping
"the same" consumer share one composite: deliveries are *split* between the
requests (each delivery handed to exactly one thread), which is the correct
competing-consumer behavior, but the one-slot `pending_error` /
`pending_signal` stashes (`composite.rs:67-76`) become cross-thread: the
one-shot `SourceReplaced` re-fetch signal can be consumed by whichever request
calls `next()` next, not the one that needs it. Degradation is bounded (the
other thread then observes a closed/retired source and re-fetches), but it
must be documented (F3). One request's `Consumer::close()` /
`__destruct` (`crates/rabbit-rs-php/src/classes/consumer.rs:249-275`) closes
the shared composite for all requests on that fingerprint — again
documented-behavior territory, not unsafety. Memory-safety-wise the composite
is atomics + mutexes throughout.

## Q2 — Tokio→PHP callbacks re-entrancy and ext-php-rs 0.16 re-MINIT

**Callbacks never run on Rust threads.** All invocation flows through
`EventBridge::drain()`, called only from PHP-thread operations
(`pool.rs:195, 222, 260, 264, 386, 595`; `consumer.rs:61, 85, 355`). The
bridge holds `Weak<ClientPool>` and two `CallbackRegistry(Mutex<Vec<Zval>>)`s
(`src/classes/bridge.rs:25-31`); spawned runtime tasks (pipelined drains, the
flush timer) touch only Rust state on `PublishBuffer`
(`src/classes/publish_buffer.rs:92-136` for the mutex/atomic fields; drain
bodies at :678-718 record errors into a Rust deque). No `Zval` is stored
anywhere a Tokio thread can reach. This matters because ext-php-rs 0.16.3
deliberately does *not* implement `Send`/`Sync` for `Zval` (non-atomic
refcounters — `src/types/zval.rs:37-41`, impls commented out with an upstream
TODO): the codebase's discipline of keeping Zend values on the PHP thread is
what makes it sound, and it holds.

**Re-entrancy.** A registered callback may re-enter the pool. Both registries
release their mutex before invoking (`callbacks.rs:81-85`; the state-diff
locks in `bridge.rs:129-150` and `bridge.rs:173-184` are likewise dropped
before callback dispatch — the same contract tested by
`tests/Pool/CallbackDeadlockTest.php`). A callback that throws is captured via
the 0.16 error model — `Error::ExceptionPending` →
`ExecutorGlobals::take_exception()` (`callbacks.rs:97-134`) — and rethrown
after the drain loop (`bridge.rs:115-121`). Executor globals are per-thread
under ZTS (TSRM), and this path runs entirely on the invoking thread; the
0.16 "throw() is a no-op while an exception is pending" behavior
(`bridge.rs:118-119` comment) is consistent with the single-threaded drain.

**Re-MINIT on worker restart.** MINIT does exactly one thing: install the
stderr sink, first-wins via the core's `OnceLock` (`lib.rs:39` →
`src/sink.rs:20-24` → `src/log.rs:75-84`), so per-thread re-MINIT is a no-op
after the first thread. No other Rust global is created at MINIT, and no
request-startup/request-shutdown handlers are registered (`lib.rs:35-56`).
The 0.15-era module-shutdown UAF (crash of the whole frankenphp process on
reload) was an upstream ext-php-rs bug fixed by 0.16 — root-caused in the
promotion-readiness investigation (internal record:
`.superpowers/sdd/2026-10-25-promotion-readiness/progress.md`, Task 7; the
bisect isolated the crash to the 0.15→0.16 boundary and nightly run
40211113591 at head `1af8375` is all green, frankenphp included). Residual:
upstream `octane:reload` (Caddy PATCH) answers 500 with the 0.16 module —
documented workaround via the admin worker-restart endpoint
(`scripts/test-octane-runtime.sh:444-457`;
`packages/laravel-queue/docs/octane.md:153, 185-187`). The one Rust-side
mutation left at thread teardown is F1 below.

## Q3 — Module globals, ZTS globals, and signals

**The module declares no ext-php-rs module globals** (`lib.rs:35-56` builds no
`.globals()`), so nothing of ours rides TSRM per-thread global slots.

**Complete inventory of process-global Rust state:**

| Item | Location | Shared under ZTS? | Safe? |
|---|---|---|---|
| `RuntimeRegistry::global()` | `runtime.rs:104-108` | yes | data-safe (Mutex); **lifecycle hazard F1** |
| Log sink `SINK` | `src/log.rs:75-84` | yes | safe: `StderrSink` is `Send + Sync`, holds no Zend state, single `write_all` per line, write errors ignored (`sink.rs:30-50`); the core's contract requires exactly this (`log.rs:20-23`). Concurrent stderr writes may interleave lines — cosmetic. First-install-wins makes per-thread re-MINIT a no-op |
| `NEXT_HANDLE_SERIAL` | `pool/mod.rs:23` | yes | atomic |
| `DECIMAL_NOTICE_SENT` | `classes/delivery.rs:227` | yes | atomic; the "once" notice fires on at most one thread instead of one per thread — cosmetic |

**Zend-value confinement.** ext-php-rs stores class state inside the Zend
object (no `Send`/`Sync` bound — `src/types/class_object.rs:21-30`), so a
`Pool`/`Consumer`/`Delivery` object and its `CallbackRegistry` `Zval`s are
reachable only from the request that owns them; `Zval` being `!Send`/`!Sync`
(upstream `zval.rs:37-41`) is satisfied by construction. All ZendMM
allocations (`PhpString`, hash tables) happen on the PHP thread.

**Signals.** Neither crate installs signal handlers or subscribes to tokio
signals (grep for `tokio::signal`, `signal_hook`, `sigaction`, `libc::signal`,
`static mut`, `lazy_static` across both crates: zero hits). Tokio's signal
driver is enabled by `enable_all` (`runtime.rs:63`) but installs its global
handler only on first signal subscription, which never happens — no conflict
with the pcntl handlers Octane's artisan commands rely on (pcntl is installed
in the derived image for exactly those signals,
`scripts/test-octane-runtime.sh:88-91`).

## Q4 — What the nightly frankenphp certification does and does not exercise

Certified (evidence):

- The embedded PHP in the pinned image is ZTS and the extension builds against
  it in-image (`scripts/test-octane-runtime.sh:369-397`; ZTS asserted at :385).
- The extension loads under ZTS and the Octane server boots with it
  (`start_frankenphp`, :400-438).
- The full no-loss scenario — publish/park → worker recycle with graceful
  flush → graceful stop with flush → external consume — passes on the 0.16
  module (`packages/laravel-queue/docs/octane.md:153`; nightly
  40211113591 green at head `1af8375`).
- Worker recycle (thread teardown → fresh thread → re-MINIT) via the admin
  endpoint, sequentially (`reload_frankenphp`, :454-457).

NOT certified — the gap between "a ZTS binary loads and serves one thread" and
"safe under production ZTS load":

- **No concurrent multi-thread load.** The harness runs `--workers=1`
  (:432). Every shared-state behavior in Q1 (N threads on one registry, one
  runtime, one `ClientPool`, one `ConsumerHandle`, concurrent
  `Handle::block_on`) is unexercised under ZTS. The prior advisory ZTS CI job
  that was removed had the same property — it only proved loading
  (`docs/plans/ROADMAP.md:1080-1085`).
- **No cross-thread MSHUTDOWN.** With one worker, a recycle tears down the
  only thread; F1 cannot manifest. With `--workers>1` and max-requests
  recycling, threads recycle at staggered times and the first one poisons the
  rest.
- **No distributed-asset exercise.** The harness still builds in-image; the
  released PIE asset is not what the nightly certifies (issue #369 acceptance
  bullet 2 covers this).

## Findings

### F1 (blocking, code) — MSHUTDOWN closes the process-global registry per thread

`crates/rabbit-rs-php/src/lib.rs:30-33`:

```rust
extern "C" fn module_shutdown(_module_type: i32, _module_number: i32) -> i32 {
    RuntimeRegistry::global().close();
    0
}
```

Under ZTS this runs at **every** worker-thread teardown. `close()` closes all
pool handles (`runtime.rs:221-236, 238-261`) — surviving threads' live `Pool`
objects then fail their `ensure_open`/closed-handle checks
(`pool.rs:714-724`, `pool/mod.rs:146-154`) — and calls
`Runtime::shutdown_timeout` (`runtime.rs:260`). Every subsequent
`handle.runtime().block_on(...)` on a surviving thread touches a dead runtime:
per tokio 1.54.1's own docs, futures touching IO resources error and **timer
futures panic** (`src/runtime/handle.rs:257-262, 322-323`). Concrete hits:
`Consumer::next` slow path (`consumer.rs:359-365`), `quiesce_within`'s
`timeout_at` (`publish_buffer.rs:409-413`), every deadline-bearing publish
path. Under NTS the same code is correct (MSHUTDOWN ≙ process end) — which is
why FPM/CLI never surfaced it.

Minimal fix: gate the close on `!ext_php_rs::PHP_ZTS` (const at ext-php-rs
0.16.3 `src/lib.rs:92`). One line plus a comment. After the fix, a ZTS worker
restart leaves the shared runtime intact and the next thread reuses it
transparently; reclamation happens at process exit.

### F2 (blocking, harness) — concurrency is unexercised

Run the frankenphp scenario with `--workers>1` and max-requests recycling
(`scripts/test-octane-runtime.sh:432`) before the release flips
`support-zts`; otherwise the first ZTS release ships a mode whose only
concurrency evidence is a single-thread run.

### F3 (required docs) — shared-client semantics under ZTS need to be stated

`docs/reference.md:574` justifies NTS-only with "ZTS binaries would share that
registry across PHP threads without synchronization" — inaccurate today: the
sharing is synchronized (Q1), and the actual deltas are behavioral: deliveries
split across requests with a shared one-shot signal stash
(`composite.rs:67-76`), a request-scoped `close()` closes the consumer for
concurrent requests on the same fingerprint (`consumer.rs:249-275`,
`pool/mod.rs:108-120`), and metrics report the process-wide pooled view
(`metrics.rs:32-35`). `packages/laravel-queue/docs/octane.md:165` already
points thread-based runtimes at the process-local registry — extend that
section instead of adding a new one.

## Non-blocking notes

- `Pool::__construct` under the registry mutex and `close_evicted_pool`
  outside it (`runtime.rs:208-215`): a thread can observe up to the shutdown
  budget of eviction latency — bounded, by design.
- Fork checks (`pid != std::process::id()` in `pool.rs:615, 645, 715`,
  `consumer.rs:250, 268`, `delivery.rs:117`) remain no-ops under ZTS (no
  fork) — harmless.
- The registry's `MAX_POOLS` eviction only takes zero-claim pools
  (`runtime.rs:277-289`), so ZTS sharing cannot have a live pool closed under
  it — the claim protocol (`pool.rs:663-665`, `pool/mod.rs:82-91`) is what
  makes cross-thread reuse safe.
- The sink's stderr lines can interleave between threads/processes; diagnostics
  only.

## Evidence base

- Code inspection at HEAD `69feee8` (read-only), all citations above.
- ext-php-rs 0.16.3 and tokio 1.54.1 vendored sources under the cargo registry
  (pinned by `Cargo.lock`).
- Harness and CI: `scripts/test-octane-runtime.sh` (frankenphp sections),
  `packages/laravel-queue/docs/octane.md` certification table, nightly run
  40211113591 (all four runtime jobs green, head `1af8375`).
- Historical decision: D8 NTS-only (`docs/plans/ROADMAP.md:1079-1085`,
  `docs/plans/2026-09-11-v1-readiness-remediation-plan.md:41`).
