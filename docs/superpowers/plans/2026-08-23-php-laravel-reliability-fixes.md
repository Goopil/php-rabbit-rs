# PHP Extension & Laravel Reliability Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 7 confirmed reliability bugs in the PHP extension and Laravel package that cause deadlocks, silent data loss, resource leaks, orphaned workers, and misleading monitoring.

**Architecture:** Extract-then-invoke pattern for callbacks, mandatory DLX validation, pool close on flush, payload validation, config propagation, supervisor cleanup, honest monitoring.

**Tech Stack:** Rust 1.96.0, ext-php-rs, PHP 8.4, Laravel 11, Pest

## Global Constraints

- `#![forbid(unsafe_code)]` — no changes to lint configuration
- `declare(strict_types=1)` in all PHP files
- TDD: write failing test first, observe failure, implement minimally, rerun
- Pest tests for PHP, Rust tests for core
- Run `rtk cargo fmt --all` after Rust edits
- Run `rtk ./scripts/check.sh` before claiming completion

## Spec Reference

`docs/superpowers/specs/2026-08-23-php-laravel-reliability-fixes-design.md`

---

## File Structure

### PHP Extension (`crates/rabbit-rs-php/src/`)

| File | Responsibility | Task |
|------|---------------|------|
| `classes/pool.rs` | Extract-then-invoke pattern for callbacks | 1 |
| `callbacks.rs` | Release `CallbackSlot` mutex before invoking PHP | 1 |

### Laravel Package (`packages/laravel-queue/src/`)

| File | Responsibility | Task |
|------|---------------|------|
| `Config/ConfigNormalizer.php` | Reject `delivery_limit` without DLX; pass `queue_type`/`queue_durable` to native config | 2, 5 |
| `Support/NativePoolFactory.php` | Call `close()` on pools before clearing cache | 3 |
| `Octane/OctaneLifecycle.php` | Close pools on flush/reload/stop | 3 |
| `Jobs/RabbitMqJob.php` | Validate `message_id` and JSON payload | 4 |
| `Console/WorkerSupervisor.php` | Stop all children on max-restarts; propagate worker options | 6 |
| `Console/RabbitMqWorkCommand.php` | Add `--timeout`, `--tries`, `--memory`, `--max-jobs`, `--max-time` options | 6 |
| `Console/RabbitMqStatusCommand.php` | Return non-zero exit code on failure; log exception | 7 |

### Config (`packages/laravel-queue/config/`)

| File | Responsibility | Task |
|------|---------------|------|
| `config/rabbit-rs.php` | Document DLX requirement when `delivery_limit` is set | 2 |

### Rust Core (`crates/rabbit-rs-core/src/`)

| File | Responsibility | Task |
|------|---------------|------|
| `config.rs` | Add `queue_type` and `queue_durable` fields to `Config` | 5 |
| `topology/plan.rs` | Use config values instead of hardcoded `QueueKind::Quorum` / `durable: true` | 5 |

---

## Task 1: Fix Callback Deadlock (Extract-Then-Invoke)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs:312-349` — `invoke_connection_state_callbacks`, `invoke_backpressure_callback`
- Modify: `crates/rabbit-rs-php/src/callbacks.rs:55-68` — `CallbackSlot::try_call`
- Test: `crates/rabbit-rs-php/tests/`

**Interfaces:**
- Consumes: nothing
- Produces: deadlock-free callback invocation

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-php/tests/`, create a Pest test that verifies a callback calling `stats()` does not deadlock:

```php
it('callback calling stats does not deadlock', function () {
    $pool = new Pool(testConfig());
    $pool->onConnectionState(function () use ($pool) {
        // Re-enter stats from inside the callback
        $pool->stats();
    });
    // Trigger a state change
    triggerConnectionState($pool);
    // If this completes without hanging, the deadlock is fixed
    $stats = $pool->stats();
    expect($stats)->toBeArray();
});
```

- [ ] **Step 2: Run test to verify it fails (hangs)**

Run: `rtk ./scripts/test-extension.sh`
Expected: FAIL — test hangs indefinitely (deadlock).

- [ ] **Step 3: Fix `invoke_connection_state_callbacks` — extract-then-invoke**

In `crates/rabbit-rs-php/src/classes/pool.rs`, replace `invoke_connection_state_callbacks`:

```rust
fn invoke_connection_state_callbacks(&self) {
    // 1. Extract callback and state data under lock
    let (callback, states) = {
        let states_guard = self
            .last_connection_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let callback = self.connection_state_callback.extract_zval();
        (callback, states_guard.clone())
    }; // Lock released here

    // 2. Invoke PHP callback WITHOUT any lock held
    if let Some(zval) = callback {
        let _ = self.connection_state_callback.try_call_unsafe(&zval, &states);
    }
}
```

Note: `extract_zval()` needs to clone the `Zval` (the callable) out of the `CallbackSlot` under lock,
then release the lock. The `try_call_unsafe` invokes the cloned Zval without holding any mutex.

- [ ] **Step 4: Fix `invoke_backpressure_callback` — same pattern**

Apply the same extract-then-invoke pattern to `invoke_backpressure_callback`.

- [ ] **Step 5: Fix `CallbackSlot::try_call` — release lock before invoke**

In `crates/rabbit-rs-php/src/callbacks.rs`, restructure `try_call` to extract the callable under lock,
release the lock, then invoke:

```rust
pub fn try_call(&self, args: ...) -> Result<(), PhpResult<()>> {
    let zval = {
        let slot = self.0.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.clone() // Clone the Option<Zval> out
    }; // Lock released

    if let Some(callable) = zval {
        callable.call(args)?;
    }
    Ok(())
}
```

Add an `extract_zval()` method that clones the Zval out under lock and returns it.

- [ ] **Step 6: Fix compilation errors**

Run: `rtk cargo build -p rabbit-rs-php`

Fix any issues with Zval cloning (Zval may not implement Clone — may need `Zval::try_clone()` or
`Zval::duplicate()`). Check the ext-php-rs API for the correct method.

- [ ] **Step 7: Run tests**

Run: `rtk ./scripts/test-extension.sh`
Expected: PASS — the callback test completes without hanging.

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/pool.rs crates/rabbit-rs-php/src/callbacks.rs crates/rabbit-rs-php/tests/
git commit -m "fix: extract-then-invoke pattern prevents callback deadlock when re-entering stats"
```

---

## Task 2: Reject `delivery_limit` Without DLX

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php:445-477` — validate DLX requirement
- Modify: `packages/laravel-queue/config/rabbit-rs.php:300-325` — document DLX requirement
- Test: `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`

- [ ] **Step 1: Write the failing test**

```php
it('rejects delivery_limit without dead_letter', function () {
    $config = validBaseConfig();
    $config['topology']['queue']['delivery_limit'] = 20;
    $config['topology']['dead_letter'] = null;

    expect(fn() => ConfigNormalizer::normalize($config))
        ->toThrow(InvalidArgumentException::class, 'dead_letter must be configured');
});

it('accepts delivery_limit with dead_letter', function () {
    $config = validBaseConfig();
    $config['topology']['queue']['delivery_limit'] = 20;
    $config['topology']['dead_letter'] = ['exchange' => 'dlx'];

    $normalized = ConfigNormalizer::normalize($config);
    expect($normalized)->toBeArray();
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: FAIL — no validation exists.

- [ ] **Step 3: Add validation in ConfigNormalizer**

After the topology normalization (around line 476):

```php
$deliveryLimit = $topology['queue']['delivery_limit'] ?? null;
$deadLetter = $topology['dead_letter'] ?? null;

if ($deliveryLimit !== null && $deadLetter === null) {
    throw new InvalidArgumentException(
        "dead_letter must be configured when delivery_limit is set — "
        . "without it, poison messages are silently dropped by the quorum queue"
    );
}
```

- [ ] **Step 4: Update config documentation**

In `config/rabbit-rs.php`, update the comment for `delivery_limit`:

```php
// 'delivery_limit' => 20,
// NOTE: dead_letter MUST be configured when delivery_limit is set.
// Without a DLX, poison messages are silently dropped after the limit is reached.
```

- [ ] **Step 5: Run tests**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/src/Config/ConfigNormalizer.php packages/laravel-queue/config/rabbit-rs.php packages/laravel-queue/tests/
git commit -m "fix: reject delivery_limit config without dead_letter to prevent silent message loss"
```

---

## Task 3: Close Pools on Flush and Fork Reset

**Files:**
- Modify: `packages/laravel-queue/src/Support/NativePoolFactory.php:58-72` — call `close()` before clearing
- Modify: `packages/laravel-queue/src/Octane/OctaneLifecycle.php:31-51` — close pools on flush/reload/stop
- Test: `packages/laravel-queue/tests/Unit/`

- [ ] **Step 1: Write the failing test**

```php
it('flush closes all pools before clearing', function () {
    $factory = new NativePoolFactory();
    $pool = $factory->make(validNativeConfig());
    
    // Verify pool is open
    expect($pool->stats()['closed'])->toBeFalse();
    
    $factory->flush();
    
    // Pool should be closed (connection terminated)
    // The pool reference is gone from the factory, but the close was called
    expect($pool->stats()['closed'])->toBeTrue();
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: FAIL — pool is not closed after flush.

- [ ] **Step 3: Fix `flush()` to close pools**

In `NativePoolFactory.php`:

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

- [ ] **Step 4: Fix `resetAfterFork()` to close pools**

```php
private function resetAfterFork(): void
{
    $processId = ($this->resolveProcessId)();
    if ($processId === $this->processId) {
        return;
    }

    foreach ($this->pools as $pool) {
        try {
            $pool->close();
        } catch (\Throwable) {
            // Best-effort
        }
    }
    $this->pools = [];
    $this->processId = $processId;
}
```

- [ ] **Step 5: Run tests**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/src/Support/NativePoolFactory.php packages/laravel-queue/tests/
git commit -m "fix: close pools before clearing cache in flush and resetAfterFork"
```

---

## Task 4: Validate Payload and message_id in RabbitMqJob

**Files:**
- Modify: `packages/laravel-queue/src/Jobs/RabbitMqJob.php:23-38` — validate before construction
- Test: `packages/laravel-queue/tests/Unit/Jobs/`

- [ ] **Step 1: Write the failing test**

```php
it('throws InvalidArgumentException when message_id is missing', function () {
    $delivery = Mockery::mock(Delivery::class);
    $delivery->shouldReceive('metadata')->andReturn(['attempts' => 0]);
    $delivery->shouldReceive('payload')->andReturn('{"job":"test"}');

    expect(fn() => new RabbitMqJob(app(), $delivery, 'rabbit-rs', 'default'))
        ->toThrow(InvalidArgumentException::class, 'message_id');
});

it('throws InvalidArgumentException when payload is invalid JSON', function () {
    $delivery = Mockery::mock(Delivery::class);
    $delivery->shouldReceive('metadata')->andReturn(['message_id' => 'abc', 'attempts' => 0]);
    $delivery->shouldReceive('payload')->andReturn('not-json');

    expect(fn() => new RabbitMqJob(app(), $delivery, 'rabbit-rs', 'default'))
        ->toThrow(InvalidArgumentException::class, 'not valid JSON');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: FAIL — `TypeError` instead of `InvalidArgumentException` for missing `message_id`.

- [ ] **Step 3: Add validation in constructor**

In `RabbitMqJob.php`:

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

- [ ] **Step 4: Run tests**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/src/Jobs/RabbitMqJob.php packages/laravel-queue/tests/
git commit -m "fix: validate message_id and JSON payload before job creation to prevent redelivery loops"
```

---

## Task 5: Pass queue.type and durable to Native Transport

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php:31-40` — add `queue_type` and `queue_durable` to native config
- Modify: `crates/rabbit-rs-core/src/config.rs:440-452` — add `queue_type` and `queue_durable` fields
- Modify: `crates/rabbit-rs-core/src/topology/plan.rs:22-31` — use config values instead of hardcoded
- Test: `crates/rabbit-rs-core/tests/topology.rs`, `packages/laravel-queue/tests/Unit/`

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/topology.rs`:

```rust
#[test]
fn queue_type_classic_from_config() {
    let config = helper::config_with_queue_type(QueueKind::Classic);
    let plan = TopologyPlan::from_config(&config);
    assert_eq!(plan.queues[0].kind, QueueKind::Classic);
}

#[test]
fn queue_durable_false_from_config() {
    let config = helper::config_with_queue_durable(false);
    let plan = TopologyPlan::from_config(&config);
    assert_eq!(plan.queues[0].durable, false);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test topology queue_type_classic`
Expected: FAIL — `Config` has no `queue_type` field.

- [ ] **Step 3: Add fields to Rust Config**

In `crates/rabbit-rs-core/src/config.rs`:

```rust
pub struct Config {
    // ... existing fields ...
    #[serde(default = "default_queue_type")]
    pub queue_type: QueueKind,
    #[serde(default = "default_true")]
    pub queue_durable: bool,
}

fn default_queue_type() -> QueueKind { QueueKind::Quorum }
fn default_true() -> bool { true }
```

- [ ] **Step 4: Use config values in topology plan**

In `crates/rabbit-rs-core/src/topology/plan.rs`:

```rust
// Before:
kind: QueueKind::Quorum,
durable: true,

// After:
kind: config.queue_type,
durable: config.queue_durable,
```

- [ ] **Step 5: Add to ConfigNormalizer native config**

In `ConfigNormalizer.php`:

```php
'native' => [
    // ... existing ...
    'queue_type' => $topology['queue']['type'],
    'queue_durable' => $topology['queue']['durable'],
],
```

- [ ] **Step 6: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test topology`
Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs crates/rabbit-rs-core/src/topology/plan.rs packages/laravel-queue/src/Config/ConfigNormalizer.php crates/rabbit-rs-core/tests/topology.rs
git commit -m "fix: pass queue type and durable from config to native transport"
```

---

## Task 6: Fix Supervisor Orphans + Propagate Worker Options

**Files:**
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php:120-128` — stop all children on max-restarts
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php:44-53` — propagate worker options
- Modify: `packages/laravel-queue/src/Console/RabbitMqWorkCommand.php:13-19` — add worker options
- Test: `packages/laravel-queue/tests/Unit/Console/`

- [ ] **Step 1: Write the failing test**

```php
it('stops all children on max-restarts', function () {
    $supervisor = new WorkerSupervisor('rabbit-rs', 'default', workers: 3, maxRestarts: 1);
    $supervisor->run();
    // Verify all child processes were stopped
    // (mock the Process objects)
    expect($supervisor->allChildrenStopped())->toBeTrue();
});

it('propagates timeout option to children', function () {
    $cmd = new RabbitMqWorkCommand();
    $input = new ArrayInput(['--timeout' => '30', '--workers' => '1'], $cmd->getDefinition());
    $supervisor = $cmd->createSupervisor($input);
    $childCmd = $supervisor->buildChildCommand(0);
    expect($childCmd)->toContain('--timeout=30');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: FAIL — children not stopped, options not propagated.

- [ ] **Step 3: Stop all children on max-restarts**

In `WorkerSupervisor.php`, line 128:

```rust
// Before:
return self::EXIT_MAX_RESTARTS;

// After:
foreach ($processes as $p) {
    if ($p->isRunning()) {
        $p->stop(10, SIGTERM);
    }
}
return self::EXIT_MAX_RESTARTS;
```

- [ ] **Step 4: Add worker options to command signature**

In `RabbitMqWorkCommand.php`:

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

- [ ] **Step 5: Propagate options in buildChildCommand**

In `WorkerSupervisor.php`:

```php
public function buildChildCommand(int $workerIndex = 0): array
{
    $cmd = [
        PHP_BINARY,
        'artisan',
        'queue:work',
        "--connection={$this->connection}",
        "--queue={$this->queue}",
        '--name=worker-'.$workerIndex,
    ];

    foreach (['timeout', 'tries', 'memory', 'max-jobs', 'max-time'] as $opt) {
        if ($this->options[$opt] !== null) {
            $cmd[] = "--{$opt}={$this->options[$opt]}";
        }
    }

    return $cmd;
}
```

- [ ] **Step 6: Check ext-pcntl availability**

In `WorkerSupervisor.php`, add a guard at the top of `run()`:

```php
if (!function_exists('pcntl_fork')) {
    throw new RuntimeException('ext-pcntl is required for the supervisor. Install it or run with --workers=1.');
}
```

- [ ] **Step 7: Run tests**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add packages/laravel-queue/src/Console/ packages/laravel-queue/tests/
git commit -m "fix: supervisor stops all children on max-restarts and propagates worker options"
```

---

## Task 7: Honest Monitoring — Non-Zero Exit on Failure

**Files:**
- Modify: `packages/laravel-queue/src/Console/RabbitMqStatusCommand.php:16-73` — return FAILURE on exception
- Test: `packages/laravel-queue/tests/Unit/Console/`

- [ ] **Step 1: Write the failing test**

```php
it('returns non-zero exit code when broker is unreachable', function () {
    $command = new RabbitMqStatusCommand();
    // Use a config that points to an unreachable broker
    $exitCode = $command->handle(new NativePoolFactory());
    expect($exitCode)->toBe(Command::FAILURE);
});

it('returns success when broker is reachable', function () {
    $command = new RabbitMqStatusCommand();
    // Use a config that points to a reachable broker
    $exitCode = $command->handle(new NativePoolFactory());
    expect($exitCode)->toBe(Command::SUCCESS);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: FAIL — always returns SUCCESS.

- [ ] **Step 3: Fix collectStats to return false on failure**

In `RabbitMqStatusCommand.php`:

```php
private function collectStats(NativePoolFactory $pools): array|false
{
    $config = $this->laravel->make('config')->get('rabbit-rs');
    if (! is_array($config)) {
        $config = [];
    }

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
    $format = $this->option('format');
    $stats = $this->collectStats($pools);

    if ($stats === false) {
        return self::FAILURE;
    }

    if ($format === 'json') {
        $this->output->write(json_encode($stats, JSON_PRETTY_PRINT));
        return self::SUCCESS;
    }

    $this->displayHuman($stats);
    return self::SUCCESS;
}
```

- [ ] **Step 4: Run tests**

Run: `rtk composer test --workdir packages/laravel-queue`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/src/Console/RabbitMqStatusCommand.php packages/laravel-queue/tests/
git commit -m "fix: status command returns non-zero exit code and logs error when stats collection fails"
```
