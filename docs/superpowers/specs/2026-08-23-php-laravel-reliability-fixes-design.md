# PHP Extension & Laravel Reliability Fixes Design

**Date:** 2026-08-23  
**Status:** Draft  
**Scope:** PHP extension bugs + Laravel package bugs (7 confirmed issues from static audit)

## Context

A static audit of the PHP extension (`crates/rabbit-rs-php/`), Laravel package (`packages/laravel-queue/`),
and CI/CD pipelines identified 7 bugs that are orthogonal to the performance gap correction plan. All 7
were verified against the exact code. They are correctness/reliability issues, not performance issues.

## Decisions

| Decision | Choice |
|----------|--------|
| Callback deadlock | Extract callable, release all mutexes, then invoke PHP callback |
| Delivery limit without DLX | Make DLX mandatory when `delivery_limit` is set; reject config otherwise |
| Pool lifecycle | Call `close()` on pools before clearing cache in `flush()` and `resetAfterFork()` |
| Payload poison | Validate `message_id` presence and JSON payload before job creation |
| Config topology | Pass `queue.type` and `durable` to the native transport config |
| Supervisor orphans | Stop all children before returning `EXIT_MAX_RESTARTS`; propagate worker options |
| Monitoring | Return non-zero exit code and log exception when stats collection fails |

## Section 1 — Deadlock réentrant des callbacks

### Problem

`pool.rs:312-348` invokes PHP callbacks while holding `std::sync::Mutex` guards (`last_connection_states`
and `last_backpressure_total`). `callbacks.rs:55-68` holds a `Mutex` guard on `CallbackSlot` during
`try_call`. If the callback calls `stats()`, `onConnectionState()`, or `onBackpressure()`, it re-enters
and deadlocks the PHP thread (FPM/Octane worker hangs permanently).

### Design

Extract the callable data **before** invoking PHP, release all mutex guards, then invoke:

```rust
fn invoke_connection_state_callbacks(&self) {
    // 1. Extract callback Zval and state data under lock
    let (callback, states) = {
        let states = self.last_connection_states.lock().unwrap();
        let callback = self.connection_state_callback.extract(); // Clone Zval if needed
        (callback, states.clone())
    }; // Lock released here
    
    // 2. Invoke PHP callback WITHOUT any lock held
    if let Some(cb) = callback {
        let _ = cb.invoke(&states);
    }
}
```

Same pattern for `invoke_backpressure_callback` and `CallbackSlot::try_call`.

### Tests

- Callback that calls `stats()` from inside the callback does not deadlock
- Callback that re-registers itself does not deadlock
- Multiple concurrent `stats()` calls from different threads do not deadlock

## Section 2 — Perte silencieuse après 20 redeliveries sans DLX

### Problem

`config/rabbit-rs.php:318-325` defaults `delivery_limit=20` with `dead_letter=null`. When a poison
message exceeds the delivery limit, a quorum queue silently drops it — it never reaches Laravel's
`failed_jobs` table. This violates the delivery contract ("silent loss is unacceptable").

### Design

Make DLX mandatory when `delivery_limit` is set:

In `ConfigNormalizer.php`:
```php
if (($topology['queue']['delivery_limit'] ?? null) !== null
    && ($topology['dead_letter'] ?? null) === null) {
    throw new InvalidArgumentException(
        "dead_letter must be configured when delivery_limit is set — "
        . "without it, poison messages are silently dropped by the quorum queue"
    );
}
```

Alternatively, provide a built-in DLX that routes to a `failed_messages` queue by default. But this is
more invasive — the config validation is the minimal fix.

### Tests

- Config with `delivery_limit=20` and `dead_letter=null` is rejected
- Config with `delivery_limit=20` and `dead_letter` set is accepted
- Config with `delivery_limit=null` and `dead_letter=null` is accepted (no limit, no DLX needed)

## Section 3 — Pools abandonnés sans fermeture

### Problem

`NativePoolFactory.php:58-72` `flush()` and `resetAfterFork()` set `$this->pools = []` without calling
`close()` on the `Pool` objects. The AMQP connections, channels, and file descriptors are leaked in
long-running workers (Octane, FrankenPHP).

### Design

Call `close()` on each pool before clearing:

```php
public function flush(): void
{
    foreach ($this->pools as $pool) {
        try {
            $pool->close();
        } catch (\Throwable) {
            // Best-effort close — pool may already be disconnected
        }
    }
    $this->pools = [];
    $this->processId = ($this->resolveProcessId)();
}
```

Same for `resetAfterFork()`.

### Tests

- `flush()` calls `close()` on all pools before clearing
- `resetAfterFork()` calls `close()` on all pools when PID changes
- Pools are not closed when PID hasn't changed (no fork)

## Section 4 — Payload poison non validé

### Problem

`RabbitMqJob.php:36` accesses `$metadata['message_id']` without checking existence. Under
`declare(strict_types=1)`, a missing `message_id` causes a `TypeError` (null → string), preventing job
construction. The message is never acked → redelivery loop → silent drop after `delivery_limit`.

Invalid JSON payloads are stored as-is and only fail when Laravel's `Job::payload()` calls
`json_decode`, which also causes a redelivery loop.

### Design

Validate in the constructor:

```php
public function __construct(
    Container $container,
    Delivery $delivery,
    string $connectionName,
    string $queue,
) {
    $metadata = $delivery->metadata();

    $messageId = $metadata['message_id'] ?? null;
    if ($messageId === null || !is_string($messageId) || $messageId === '') {
        throw new InvalidArgumentException(
            "Delivery is missing required 'message_id' metadata — cannot create job"
        );
    }

    $rawBody = $delivery->payload();
    $payload = json_decode($rawBody, true);
    if (json_last_error() !== JSON_ERROR_NONE) {
        throw new InvalidArgumentException(
            "Delivery payload is not valid JSON: " . json_last_error_msg()
        );
    }

    $this->container = $container;
    $this->delivery = $delivery;
    $this->connectionName = $connectionName;
    $this->queue = $queue;
    $this->rawBody = $rawBody;
    $this->jobId = $messageId;
    $this->deliveryAttempts = (int) ($metadata['attempts'] ?? 0);
}
```

The worker catches `InvalidArgumentException` and fails the job explicitly (ACK + log), preventing
the redelivery loop.

### Tests

- Missing `message_id` throws `InvalidArgumentException` (not `TypeError`)
- Invalid JSON payload throws `InvalidArgumentException`
- Valid payload with `message_id` constructs the job successfully

## Section 5 — Config topology partiellement morte

### Problem

`ConfigNormalizer.php:31-40` does not pass `queue.type` and `queue.durable` to the `native` config array.
The Rust `Config` struct (`config.rs:440-452`) has no fields for them. Queue type and durability are
hardcoded in the topology plan builder (`plan.rs:22-31`: `QueueKind::Quorum`, `durable: true`).

### Design

Add `queue_type` and `queue_durable` to the Rust `Config` struct and the `native` config array:

In `config.rs`:
```rust
pub struct Config {
    // ... existing fields ...
    #[serde(default = "default_queue_type")]
    pub queue_type: QueueKind,
    #[serde(default = "default_true")]
    pub queue_durable: bool,
}
```

In `ConfigNormalizer.php`:
```php
'native' => [
    // ... existing ...
    'queue_type' => $topology['queue']['type'],
    'queue_durable' => $topology['queue']['durable'],
],
```

In `plan.rs`, use the config values instead of hardcoded constants.

### Tests

- Config with `type=classic` creates a classic queue (not quorum)
- Config with `durable=false` creates a transient queue
- Default config still creates `quorum` + `durable=true`

## Section 6 — Supervisor laissant des workers orphelins

### Problem

`WorkerSupervisor.php:128` returns `EXIT_MAX_RESTARTS` immediately when one worker exceeds max-restarts,
without stopping other running children. The other workers are orphaned.

`RabbitMqWorkCommand.php:13-19` and `WorkerSupervisor.php:44-53` don't propagate `--timeout`, `--tries`,
`--memory`, `--max-jobs`, `--max-time` to child `queue:work` processes.

### Design

Stop all children before returning:

```php
} else {
    // Max restarts reached — stop all children before exiting
    foreach ($processes as $p) {
        if ($p->isRunning()) {
            $p->stop(10, SIGTERM);
        }
    }
    return self::EXIT_MAX_RESTARTS;
}
```

Add worker options to the command signature and propagate:

```php
protected $signature = 'rabbit-rs:work
    {--connection=rabbit-rs : The queue connection name}
    {--queue=default : The queue/profile name}
    {--workers=1 : Number of child workers}
    {--max-restarts=3 : Maximum restarts per worker}
    {--backoff=1 : Base backoff in seconds}
    {--timeout=60 : The number of seconds a child process can run}
    {--tries= : Number of times to attempt a job before failing it}
    {--memory=128 : The memory limit in megabytes}
    {--max-jobs= : The number of jobs to process before stopping}
    {--max-time= : The maximum number of seconds the worker should run}
    {--rabbit-rs-worker= : Worker index for logging/metrics attribution}';
```

In `buildChildCommand()`, forward these options:
```php
foreach (['--timeout', '--tries', '--memory', '--max-jobs', '--max-time'] as $opt) {
    if ($this->option($opt) !== null) {
        $cmd[] = "--{$opt}={$this->option($opt)}";
    }
}
```

Also add `ext-pcntl` to `composer.json` `suggest` or `require`.

### Tests

- At max-restarts: all children are stopped before the supervisor exits
- `--timeout=30` is propagated to child `queue:work` processes
- Missing `ext-pcntl` produces a clear error message

## Section 7 — Monitoring mensonger

### Problem

`RabbitMqStatusCommand.php:46-70` catches any `Throwable` and returns a snapshot of zeros with
`self::SUCCESS` (exit code 0). A broker outage appears healthy.

### Design

Return a non-zero exit code and log the exception:

```php
private function collectStats(NativePoolFactory $pools): array|false
{
    // ...
    try {
        $normalized = ConfigNormalizer::normalize($config);
        $pool = $pools->make($normalized['native']);
        return $pool->stats();
    } catch (\Throwable $e) {
        $this->error("Failed to collect stats: " . $e->getMessage());
        return false;
    }
}

public function handle(NativePoolFactory $pools): int
{
    $stats = $this->collectStats($pools);
    if ($stats === false) {
        return self::FAILURE;
    }
    // ... display stats ...
    return self::SUCCESS;
}
```

### Tests

- Broker unreachable: exit code is non-zero
- Exception message is displayed
- Broker reachable: exit code is 0 and stats are displayed

## Implementation Order

1. **Section 1** (callback deadlock) — critical, can hang production workers
2. **Section 2** (delivery limit + DLX) — critical, silent data loss
3. **Section 4** (payload poison) — critical, redelivery loop
4. **Section 3** (pool lifecycle) — high, resource leak
5. **Section 6** (supervisor orphans) — high, orphaned workers
6. **Section 7** (monitoring) — high, misleading status
7. **Section 5** (config topology) — medium, config not honored

## Out of Scope

- Performance optimizations (covered in the performance gap correction plan)
- CI/CD pipeline fixes (qualification PHP, release sync, glibc baseline) — separate effort
- Stub completeness and reflection test coverage — separate effort
