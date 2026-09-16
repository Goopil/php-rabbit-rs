# Post-0.3.6 roast fixes: #310, #308, #309

Three bugs opened 2026-09-15 after the v0.3.6 playground roast (toxiproxy
12 s network cut on a quorum queue, RabbitMQ 4.2.9). Root causes are
explored and verified against the code; this plan sequences three
independent PRs sharing one validation scenario.

## PR 1 — #310: env JSON prefetch + silent child death

**Root cause (verified).** `ConnectionCompiler::prefetch()`
(`packages/laravel-queue/src/Config/ConnectionCompiler.php:416-420`) routes
every string — including the documented
`RABBIT_RS_PREFETCH='{"mode":"adaptive",...}'` — to `positiveInt()`, which
fails the integer regex and throws
`InvalidArgumentException: queue.connections.<name>.subscriptions.default.prefetch: must be an integer or an env-style integer string`.
The child `queue:work` dies before consuming; the supervisor captures child
stdout/stderr with no output callback (`WorkerSupervisor.php:818-832`) and
one-shot mode reports the plan as drained without printing anything
(`runOneShot()`, `WorkerSupervisor.php:477-533`). The parent-side depth
sampler swallows the identical compile error
(`QueueDepthSampler.php:224-226`, silent `null`).

**Fix.**
1. `prefetch()` string branch: if `str_starts_with(trim($prefetch), '{')`,
   `json_decode(..., true, 512)` and fall through to the existing
   fixed/adaptive array validation (bounds, `early_ack`/`no_ack` rejection).
   Non-array or malformed JSON → `invalid($path, ...)` with the prefetch
   path (no raw `JsonException`).
2. Loud failure: `runOneShot()` and `runInline()` surface the child's
   `getErrorOutput()` on non-zero exit through the command output; log at
   the sampler catch instead of caching a silent null.
3. Tests (TDD): JSON string → adaptive form; JSON string → fixed form;
   malformed JSON rejected with the prefetch path; bounds and
   `early_ack`/`no_ack` still enforced on the decoded array; once-mode
   feature test asserting the crashed child's stderr is surfaced.

**Files.** `src/Config/ConnectionCompiler.php`,
`src/Console/WorkerSupervisor.php`, `src/Support/QueueDepthSampler.php`,
`tests/Unit/ConnectionCompilerTest.php`,
`tests/Feature/WorkerSupervisorIntegrationTest.php`.
The stale bench vendor copy is build-managed; ignore it.

## PR 2 — #308: stop-when-empty abandons the in-flight window

**Root cause (verified).** The emptiness signal is ready-only:
`ManagementApi::queueDepth` reads `messages_ready` only
(`ManagementApi.php:59`), the native fallback is a passive declare
(`message_count` = ready), `RabbitMqQueue::reservedSize()` is hardcoded 0.
At the cut boundary the child exits 0 (a pop throw becomes `null` →
`QueueEmpty` in Laravel core), the supervisor probes ≤100 ms later, reads
`ready=0` truthfully (the broker requeues the 12 unacked window messages
only ~10 s later), the #287 fresh re-probe also reads 0 → break. The
re-arm did not misfire: its signal did not exist yet.

**Fix.**
1. Convergence window: in `runOneShot()`, after `$slots === []`, poll fresh
   depth every ~1 s for up to ~15 s (covers the ~10 s quorum requeue lag)
   before concluding drained; `pending > 0` at any tick → re-arm
   immediately. The #287 fresh re-probe becomes the first tick.
2. Include `messages_unacknowledged` in the management gauge so re-arms
   fire before the broker requeues (one line, same response shape).
3. Tests (TDD): a fake depth source that returns 0 for the first K fresh
   probes then the real depth → assert re-arm happens; extend the existing
   `#287` fresh-re-probe test. Validation: the roast scenario
   (300 jobs, proxy cut 12 s, `--stop-when-empty`) must drain 100 % with
   exit 0 and zero stuck messages.

**Files.** `src/Console/WorkerSupervisor.php`,
`src/Support/ManagementApi.php`, `tests/Feature/WorkerSupervisorIntegrationTest.php`.

## PR 3 — #309: closed-set pop retry storm

**Root cause (verified).** No retry loop exists in the package's pop path —
`RabbitMqQueue::pop()` evicts the handle and rethrows
(`RabbitMqQueue.php:518-523`). The storm is Laravel core's
`Worker::getNextJob` catch (report → sleep 1 s → sleep → re-pop,
unbounded) multiplied by supervisor re-arms and worker processes; each hit
is one ERROR entry Laravel's `report()` emits for the thrown
`QueueException`. The package cannot intercept `report()`, so the lever is
throw frequency.

**Fix.**
1. In the `NativeException` catch of `pop()` (single choke point, both base
   and Horizon variants route through it): on a closed-set refusal, evict
   as today, then retry the native fetch inline with a small bounded
   backoff (2 re-fetches, ~250 ms) before throwing. Bounded sleep is
   acceptable here (post-recovery path, not the hot dispatch loop).
2. Keep throwing on persistent Closed — stop-when-empty keeps its signal;
   the #308 convergence window makes the supervisor re-arm reliable.
3. Tests (TDD): Unit test with a fake consumer that returns Closed once
   then serves — assert pop recovers inline without throwing; fake that
   stays Closed — assert exactly one throw after the bounded retries.
   Validation: shared roast scenario — zero `consumer set is closed`
   entries in laravel.log (vs 889).

**Files.** `src/RabbitMqQueue.php`, `tests/Unit/RabbitMqQueuePopTest.php`.
Rust-side post-acquisition liveness in `Client::consumer()` is a follow-up
issue if the storm survives the roast.

## Validation

The roast scenario (issue recipe: 300 jobs, proxied consumer, 12 s cut,
`--stop-when-empty`) runs locally after each PR: full drain, exit 0,
clean logs. Full gate (`rtk ./scripts/check.sh`) plus the Laravel battery
(`rtk ./scripts/test-laravel.sh`) before each PR.

## Ordering and shipping

1. PR 1 (#310) from `main` — independent.
2. PR 2 (#308) from `main` — supervisor files only.
3. PR 3 (#309) from `main` — queue file only; rebase if conflicts.
4. Joint roast validation; fixes ship as v0.3.7.
