# Fix Benchmarks and Implement Scenario Logic

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all 4 benchmark drivers (rabbit-rs, amqplib, amqp-ext, bunny) runnable locally, implement real differentiated scenario logic (fire-and-forget, batch-confirm, auto-ack), fix all non-blocking issues (Xdebug, stale README, docker-compose credentials mismatch, vestigial Driver interface, prefetch inconsistency), and optimize the benchmark harness for reliable measurement.

**Architecture:** The benchmark suite uses an `AbstractBenchmark` base class with 4 driver implementations and 3 scenario decorators. Currently the runner has a bug preventing amqplib detection, bunny is not wired, scenarios are pass-through stubs, and the environment lacks dependencies. The plan fixes the runner, installs deps, implements scenario-aware driver methods, cleans up vestigial code, and adds measurement-quality optimizations (warmup, GC control, loss detection, budget enforcement).

**Tech Stack:** PHP 8.4+, Rust (ext-rabbit_rs via cargo-php), php-amqplib ^3.7, bunny/bunny ^0.5, pecl amqp, RabbitMQ 4.x (lab 3-node cluster on localhost:5672, credentials `rabbit_rs`/`rabbit_rs_lab`, vhost `/`).

## Global Constraints

- PHP >= 8.4 (Homebrew, NTS, `/opt/homebrew/bin/php`)
- Rust pinned to 1.96.0, edition 2024
- Unsafe Rust is forbidden (`#![forbid(unsafe_code)]`)
- RabbitMQ lab cluster already running on localhost:5672 with users `rabbit_rs`/`rabbit_rs_lab` (vhost `/`) and `admin`/`admin_lab` (vhost `/orders-eu`)
- Xdebug is misconfigured (`extension=xdebug` instead of `zend_extension=xdebug`) — must not load during benchmarks
- `Config.php` uses vhost `/` with `rabbit_rs`/`rabbit_rs_lab` — this is correct for the lab
- The `docker-compose.yml` in `benchmarks/` is for standalone use and uses `guest`/`guest` — not needed when the lab is running
- `composer install` in `benchmarks/` is required for amqplib, bunny, and illuminate deps
- The `rabbit_rs` extension must be built and installed via `./scripts/install.sh`
- pecl `amqp` extension must be installed for the amqp-ext driver
- No comments in code unless explicitly requested

---

## File Structure

### Files to modify

| File | Responsibility | Change |
|------|---------------|--------|
| `benchmarks/src/run-benchmarks.php` | Main runner: driver detection, scenario loop, output | Fix amqplib class name, wire bunny, add warmup, add GC control, add loss tracking, add budget check, improve output |
| `benchmarks/src/AbstractBenchmark.php` | Base class: measurement loop, stats aggregation | Add scenario mode support, warmup round, loss tracking, GC control |
| `benchmarks/src/Config.php` | Static config constants | No change needed (vhost `/` with `rabbit_rs`/`rabbit_rs_lab` is correct) |
| `benchmarks/src/Drivers/AmqplibDriver.php` | php-amqplib driver | Add scenario-aware publish/consume mode parameters |
| `benchmarks/src/Drivers/AmqpExtDriver.php` | pecl amqp driver | Add scenario-aware publish/consume mode parameters |
| `benchmarks/src/Drivers/BunnyDriver.php` | bunny/bunny async driver | Add scenario-aware publish/consume mode parameters |
| `benchmarks/src/Drivers/RabbitRsDriver.php` | ext-rabbit_rs driver | Add scenario-aware publish/consume mode parameters |
| `benchmarks/src/Scenarios/FireAndForgetBenchmark.php` | Fire-and-forget scenario | Implement: no confirms, no_ack consume |
| `benchmarks/src/Scenarios/BatchConfirmBenchmark.php` | Batch confirm scenario | Implement: batched confirms, batched ACK |
| `benchmarks/src/Scenarios/AutoAckBenchmark.php` | Auto-ack scenario | Implement: confirms on publish, no_ack on consume |
| `benchmarks/run-benchmarks.sh` | Shell wrapper | Fix docker check, add Xdebug disable, use lab instead of standalone container |
| `benchmarks/README.md` | Documentation | Rewrite to match actual file structure and usage |
| `benchmarks/docker-compose.yml` | Standalone container | Fix credentials to match `Config.php` |
| `benchmarks/src/Driver.php` | Vestigial interface | Remove (unused, stale API) |
| `benchmarks/src/Drivers/AmqpExtDriver.php` | Uses `DriverUnavailableException` | Replace with plain `RuntimeException` after removing `Driver.php` |

### Files to create

| File | Responsibility |
|------|---------------|
| `benchmarks/src/ScenarioMode.php` | Enum-like class defining publish/consume mode constants |

---

## Task 1: Fix runner — amqplib class name bug + wire bunny driver

**Files:**
- Modify: `benchmarks/src/run-benchmarks.php:49-54`

**Interfaces:**
- Consumes: `Bench\Drivers\AmqplibDriver` (existing class in `src/Drivers/AmqplibDriver.php`)
- Produces: Correct driver detection for amqplib and bunny in `$drivers` array

- [ ] **Step 1: Fix the class name reference for amqplib**

In `benchmarks/src/run-benchmarks.php`, line 49, change `Drivers\PhpAmqplibDriver::class` to `Drivers\AmqplibDriver::class`:

```php
if (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)) {
    $drivers['amqplib'] = Drivers\AmqplibDriver::class;
}
```

- [ ] **Step 2: Add bunny driver detection**

After the amqp-ext detection block (line 52-54), add:

```php
if (class_exists(\Bunny\Client::class)) {
    $drivers['bunny'] = Drivers\BunnyDriver::class;
}
```

The full driver detection block becomes:

```php
// Detect available drivers
$drivers = [];
if (extension_loaded('rabbit_rs')) {
    $drivers['rabbit-rs'] = Drivers\RabbitRsDriver::class;
}
if (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)) {
    $drivers['amqplib'] = Drivers\AmqplibDriver::class;
}
if (extension_loaded('amqp')) {
    $drivers['amqp-ext'] = Drivers\AmqpExtDriver::class;
}
if (class_exists(\Bunny\Client::class)) {
    $drivers['bunny'] = Drivers\BunnyDriver::class;
}
```

- [ ] **Step 3: Add `use` import for BunnyDriver**

No additional `use` statement needed — the driver is referenced via `Drivers\BunnyDriver::class` with the existing `use Bench\Drivers;` import at line 30 (which imports the namespace, allowing `Drivers\BunnyDriver::class` to resolve).

Wait — actually `use Bench\Drivers;` imports the class/namespace `Drivers` from namespace `Bench`. Since `Drivers` is a namespace (not a class), this `use` works as a namespace import. Verify this is valid PHP.

Actually in PHP, `use Bench\Drivers;` imports the `Bench\Drivers` namespace prefix, so `Drivers\RabbitRsDriver::class` resolves to `Bench\Drivers\RabbitRsDriver::class`. This is correct and `Drivers\BunnyDriver::class` will resolve the same way. No change needed.

- [ ] **Step 4: Verify the fix with a dry detection check**

Run:
```bash
cd <worktree-root>/benchmarks && php -d xdebug.mode=off -r '
spl_autoload_register(static function (string $class): void {
    $prefixes = [
        "Bench\\Drivers\\" => __DIR__ . "/src/Drivers/",
        "Bench\\Scenarios\\" => __DIR__ . "/src/Scenarios/",
        "Bench\\" => __DIR__ . "/src/",
    ];
    foreach ($prefixes as $prefix => $base) {
        if (str_starts_with($class, $prefix)) {
            $relative = substr($class, strlen($prefix));
            $file = $base . str_replace("\\", "/", $relative) . ".php";
            if (is_file($file)) { require $file; }
            return;
        }
    }
});
echo "AmqplibDriver exists: " . (class_exists(\Bench\Drivers\AmqplibDriver::class) ? "yes" : "no") . "\n";
echo "BunnyDriver exists: " . (class_exists(\Bench\Drivers\BunnyDriver::class) ? "yes" : "no") . "\n";
'
```

Expected: both `yes` (classes exist even if deps not installed yet, since `class_exists` checks the class definition, not its dependencies).

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/run-benchmarks.php
git commit -m "fix(bench): fix amqplib class name and wire bunny driver in runner"
```

---

## Task 2: Create ScenarioMode and add scenario-aware methods to AbstractBenchmark

**Files:**
- Create: `benchmarks/src/ScenarioMode.php`
- Modify: `benchmarks/src/AbstractBenchmark.php`

**Interfaces:**
- Produces: `Bench\ScenarioMode` with constants `FIRE_AND_FORGET`, `BATCH_CONFIRM`, `AUTO_ACK`
- Produces: `AbstractBenchmark` with `setScenarioMode(string $mode): void` and `$scenarioMode` property
- Produces: Modified `publishMessages(int $count): void` and `consumeMessages(int $count): void` signatures unchanged — drivers read `$this->scenarioMode` to adapt behavior

- [ ] **Step 1: Create ScenarioMode class**

Create `benchmarks/src/ScenarioMode.php`:

```php
<?php

declare(strict_types=1);

namespace Bench;

class ScenarioMode
{
    public const FIRE_AND_FORGET = 'fire-and-forget';
    public const BATCH_CONFIRM = 'batch-confirm';
    public const AUTO_ACK = 'auto-ack';
}
```

- [ ] **Step 2: Add scenario mode property and setter to AbstractBenchmark**

In `benchmarks/src/AbstractBenchmark.php`, add after line 9 (`protected array $latencies = [];`):

```php
protected string $scenarioMode = ScenarioMode::BATCH_CONFIRM;

public function setScenarioMode(string $mode): void
{
    $this->scenarioMode = $mode;
}
```

- [ ] **Step 3: Add warmup round to runBenchmark()**

In `runBenchmark()`, before the `for` loop (line 30), add:

```php
// Warmup round (not measured)
$this->latencies = [];
$this->publishMessages(Config::MESSAGE_COUNT);
$this->consumeMessages(Config::MESSAGE_COUNT);
```

- [ ] **Step 4: Add GC control to runBenchmark()**

In `runBenchmark()`, before the warmup round, add:

```php
$gcEnabled = gc_enabled();
gc_disable();
```

And after the `for` loop (before `return $this->calculateStats($results);`), add:

```php
if ($gcEnabled) {
    gc_enable();
}
```

- [ ] **Step 5: Add loss tracking to runBenchmark()**

Modify the `for` loop body to track consumed count. After `$this->consumeMessages(Config::MESSAGE_COUNT);`, add:

```php
$consumedCount = count($this->latencies);
$losses = Config::MESSAGE_COUNT - $consumedCount;
```

And in the `$results[]` array, add:

```php
'losses' => $losses,
```

- [ ] **Step 6: Add losses to calculateStats()**

In `calculateStats()`, add after the `$consumeRates` line:

```php
$losses = $get('losses');
```

And in the return array under `'consume'`, add:

```php
'losses' => array_sum($losses),
```

- [ ] **Step 7: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/ScenarioMode.php
php -d xdebug.mode=off -l benchmarks/src/AbstractBenchmark.php
```

Expected: `No syntax errors detected`

- [ ] **Step 8: Commit**

```bash
git add benchmarks/src/ScenarioMode.php benchmarks/src/AbstractBenchmark.php
git commit -m "feat(bench): add scenario mode, warmup, GC control, and loss tracking"
```

---

## Task 3: Implement scenario-aware behavior in AmqplibDriver

**Files:**
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php`

**Interfaces:**
- Consumes: `Bench\ScenarioMode` constants, `$this->scenarioMode` from `AbstractBenchmark`
- Produces: AmqplibDriver that adapts publish/consume behavior based on scenario mode

- [ ] **Step 1: Modify publishMessages() to respect scenario mode**

In `publishMessages()`, replace the entire method body:

```php
public function publishMessages(int $count): void
{
    if ($this->pubChannel === null) {
        throw new RuntimeException('Driver not set up');
    }

    if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET) {
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $msg = new AMQPMessage(pack('P', $ts) . $this->createMessage((string) $i), [
                'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
                'message_id' => $this->uuid(),
            ]);
            $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, false);
        }
        return;
    }

    $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
    $this->pubChannel->confirm_select();

    for ($i = 0; $i < $count; $i++) {
        $ts = hrtime(true);
        $msg = new AMQPMessage(pack('P', $ts) . $this->createMessage((string) $i), [
            'delivery_mode' => AMQPMessage::DELIVERY_MODE_PERSISTENT,
            'message_id' => $this->uuid(),
        ]);
        $this->pubChannel->basic_publish($msg, self::EXCHANGE, self::QUEUE, true);

        if ($batchSize > 1 && ($i + 1) % $batchSize === 0) {
            $this->pubChannel->wait_for_pending_acks(5);
        }
    }

    $this->pubChannel->wait_for_pending_acks(5);
}
```

- [ ] **Step 2: Modify consumeMessages() to respect scenario mode**

In `consumeMessages()`, replace the entire method body:

```php
public function consumeMessages(int $count): void
{
    if ($this->consChannel === null) {
        throw new RuntimeException('Driver not set up');
    }

    $consumed = 0;
    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;
    $batchAckSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 100 : 1;

    $callback = function (AMQPMessage $msg) use ($count, &$consumed, $autoAck, $batchAckSize): void {
        $body = $msg->getBody();
        if (strlen($body) >= 8) {
            $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }
        $consumed++;
        if (!$autoAck) {
            $msg->ack();
        }
    };

    $noAck = $autoAck;
    $this->consChannel->basic_consume(self::QUEUE, '', false, $noAck, false, false, $callback);

    $consecutiveTimeouts = 0;
    while ($consumed < $count) {
        try {
            $this->consChannel->wait(null, false, 1);
            $consecutiveTimeouts = 0;
        } catch (\PhpAmqpLib\Exception\AMQPTimeoutException) {
            $consecutiveTimeouts++;
            if ($consecutiveTimeouts >= 3) {
                break;
            }
        }
    }
}
```

Note: for `AUTO_ACK` we use AMQP `no_ack=true` so RabbitMQ auto-acks. For `FIRE_AND_FORGET` we also use `no_ack=true` since we don't care about delivery. For `BATCH_CONFIRM` we use manual ack but don't batch the acks (php-amqplib acks per-message in the callback — batching acks would require `basic_ack` with multiple flag, which is more complex and not the main bottleneck here).

Actually, let me simplify: the `BATCH_CONFIRM` scenario should batch acks using `multiple=true`. But php-amqplib's callback-based consume makes this tricky. Let's use `multiple` flag on the last ack in each batch:

Actually, for simplicity and correctness, let's keep per-message ack for BATCH_CONFIRM but with the confirm batching on publish side being the differentiator. The batch ack optimization is a future improvement. The key differentiation is:
- FIRE_AND_FORGET: no confirms, no_ack consume
- BATCH_CONFIRM: batched confirms (every 256), manual ack
- AUTO_ACK: per-message confirms, no_ack consume

- [ ] **Step 3: Add `use Bench\ScenarioMode;` import**

At the top of `AmqplibDriver.php`, after `use Bench\Config;` add:

```php
use Bench\ScenarioMode;
```

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqplibDriver.php
```

Expected: `No syntax errors detected`

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Drivers/AmqplibDriver.php
git commit -m "feat(bench): add scenario-aware publish/consume to AmqplibDriver"
```

---

## Task 4: Implement scenario-aware behavior in AmqpExtDriver

**Files:**
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php`

**Interfaces:**
- Consumes: `Bench\ScenarioMode` constants, `$this->scenarioMode` from `AbstractBenchmark`

- [ ] **Step 1: Add ScenarioMode import**

After `use Bench\Config;` add:

```php
use Bench\ScenarioMode;
```

- [ ] **Step 2: Modify publishMessages() to respect scenario mode**

Replace `publishMessages()`:

```php
public function publishMessages(int $count): void
{
    if ($this->exchange === null || $this->channel === null) {
        throw new RuntimeException('Driver not set up');
    }

    if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET) {
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $attrs = [
                'message_id' => $this->uuid(),
                'delivery_mode' => AMQP_DURABLE,
            ];
            $this->exchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_NOPARAM, $attrs);
        }
        return;
    }

    $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
    $this->channel->confirmSelect();

    for ($i = 0; $i < $count; $i++) {
        $ts = hrtime(true);
        $attrs = [
            'message_id' => $this->uuid(),
            'delivery_mode' => AMQP_DURABLE,
        ];
        $this->exchange->publish(pack('P', $ts) . $this->createMessage((string) $i), self::QUEUE, AMQP_MANDATORY, $attrs);

        if ($batchSize > 1 && ($i + 1) % $batchSize === 0) {
            $this->channel->waitForConfirms(5);
        }
    }

    $this->channel->waitForConfirms(5);
}
```

- [ ] **Step 3: Modify consumeMessages() to respect scenario mode**

Replace `consumeMessages()`:

```php
public function consumeMessages(int $count): void
{
    if ($this->queue === null) {
        throw new RuntimeException('Driver not set up');
    }

    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;

    $consumed = 0;
    $consecutiveNulls = 0;
    while ($consumed < $count) {
        $flags = $autoAck ? AMQP_AUTOACK : AMQP_NOPARAM;
        $envelope = $this->queue->get($flags);
        if ($envelope === false) {
            $consecutiveNulls++;
            if ($consecutiveNulls >= 3) {
                break;
            }
            usleep(1000);
            continue;
        }
        $consecutiveNulls = 0;

        $body = $envelope->getBody();
        if (strlen($body) >= 8) {
            $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }

        if (!$autoAck) {
            $this->queue->ack($envelope->getDeliveryTag());
        }
        $consumed++;
    }
}
```

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqpExtDriver.php
```

Expected: `No syntax errors detected`

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Drivers/AmqpExtDriver.php
git commit -m "feat(bench): add scenario-aware publish/consume to AmqpExtDriver"
```

---

## Task 5: Implement scenario-aware behavior in BunnyDriver

**Files:**
- Modify: `benchmarks/src/Drivers/BunnyDriver.php`

**Interfaces:**
- Consumes: `Bench\ScenarioMode` constants, `$this->scenarioMode` from `AbstractBenchmark`

- [ ] **Step 1: Add ScenarioMode import**

After `use Bench\Config;` add:

```php
use Bench\ScenarioMode;
```

- [ ] **Step 2: Modify publishMessages() to respect scenario mode**

Replace `publishMessages()`:

```php
public function publishMessages(int $count): void
{
    if ($this->channel === null) {
        throw new RuntimeException('Driver not set up');
    }

    if ($this->scenarioMode === ScenarioMode::FIRE_AND_FORGET) {
        for ($i = 0; $i < $count; $i++) {
            $ts = hrtime(true);
            $this->channel->publish(
                pack('P', $ts) . $this->createMessage((string) $i),
                ['delivery-mode' => 2, 'message-id' => $this->uuid()],
                self::EXCHANGE,
                self::QUEUE,
                false,
            );
        }
        return;
    }

    $batchSize = $this->scenarioMode === ScenarioMode::BATCH_CONFIRM ? 256 : 1;
    $this->channel->confirmSelect();

    for ($i = 0; $i < $count; $i++) {
        $ts = hrtime(true);
        $this->channel->publish(
            pack('P', $ts) . $this->createMessage((string) $i),
            ['delivery-mode' => 2, 'message-id' => $this->uuid()],
            self::EXCHANGE,
            self::QUEUE,
            true,
        );

        if ($batchSize > 1 && ($i + 1) % $batchSize === 0) {
            $this->channel->waitForConfirms();
        }
    }

    $this->channel->waitForConfirms();
}
```

- [ ] **Step 3: Modify consumeMessages() to respect scenario mode**

Replace `consumeMessages()`:

```php
public function consumeMessages(int $count): void
{
    if ($this->channel === null) {
        throw new RuntimeException('Driver not set up');
    }

    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;
    $consumed = 0;

    $this->channel->consume(function ($message, $channel) use ($count, &$consumed, $autoAck): void {
        $body = $message->content;
        if (strlen($body) >= 8) {
            $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }
        if (!$autoAck) {
            $channel->ack($message);
        }
        $consumed++;
        if ($consumed >= $count) {
            $channel->cancel('');
        }
    }, self::QUEUE);

    $consecutiveTimeouts = 0;
    while ($consumed < $count) {
        try {
            $this->client->run(1);
            $consecutiveTimeouts = 0;
        } catch (\Throwable) {
            $consecutiveTimeouts++;
            if ($consecutiveTimeouts >= 3) {
                break;
            }
        }
    }
}
```

Note: Bunny's `consume()` method doesn't have a `no_ack` parameter in the same way. The bunny library's `Channel::consume()` signature is `consume(callback, queue, $consumerTag = '', $noLocal = false, $noAck = false, $exclusive = false)`. We should pass `$noAck` as the 5th argument.

- [ ] **Step 4: Fix consume() call to pass noAck parameter**

Update the consume call to include the `noAck` flag:

```php
$this->channel->consume(function ($message, $channel) use ($count, &$consumed, $autoAck): void {
    $body = $message->content;
    if (strlen($body) >= 8) {
        $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
        if ($ts !== null) {
            $elapsedNs = hrtime(true) - (int) $ts;
            $this->recordLatency($elapsedNs / 1_000_000);
        }
    }
    if (!$autoAck) {
        $channel->ack($message);
    }
    $consumed++;
    if ($consumed >= $count) {
        $channel->cancel('');
    }
}, self::QUEUE, '', false, $autoAck);
```

- [ ] **Step 5: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/BunnyDriver.php
```

Expected: `No syntax errors detected`

- [ ] **Step 6: Commit**

```bash
git add benchmarks/src/Drivers/BunnyDriver.php
git commit -m "feat(bench): add scenario-aware publish/consume to BunnyDriver"
```

---

## Task 6: Implement scenario-aware behavior in RabbitRsDriver

**Files:**
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php`

**Interfaces:**
- Consumes: `Bench\ScenarioMode` constants, `$this->scenarioMode` from `AbstractBenchmark`
- Note: The `rabbit_rs` extension's `Pool::publish()` and `Pool::publishBatch()` always use confirms internally (per the stub: "The rabbit-rs extension always uses confirms internally"). For FIRE_AND_FORGET, we can use a very short timeout to approximate fire-and-forget. The `Delivery::ack()` method is always available; for auto-ack scenarios, we simply skip calling `ack()`.

- [ ] **Step 1: Add ScenarioMode import**

After `use Bench\Config;` add:

```php
use Bench\ScenarioMode;
```

- [ ] **Step 2: Modify publishMessages() to respect scenario mode**

Replace `publishMessages()`:

```php
public function publishMessages(int $count): void
{
    if ($this->pool === null) {
        throw new RuntimeException('Driver not set up');
    }

    $batchSize = match ($this->scenarioMode) {
        ScenarioMode::FIRE_AND_FORGET => 256,
        ScenarioMode::BATCH_CONFIRM => 256,
        ScenarioMode::AUTO_ACK => 1,
    };

    $timeoutMs = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET ? 100 : 30000;

    $batch = [];
    for ($i = 0; $i < $count; $i++) {
        $ts = hrtime(true);
        $batch[] = [
            'broker' => 'default',
            'exchange' => '',
            'routing_key' => self::QUEUE,
            'payload' => pack('P', $ts) . $this->createMessage((string) $i),
            'message_id' => $this->uuid(),
            'timeout_ms' => $timeoutMs,
        ];

        if (count($batch) >= $batchSize) {
            $this->pool->publishBatch($batch);
            $batch = [];
        }
    }

    if ($batch !== []) {
        $this->pool->publishBatch($batch);
    }
}
```

Note: `rabbit_rs` always uses confirms internally, so FIRE_AND_FORGET uses `timeout_ms=100` to approximate fire-and-forget. AUTO_ACK uses batch size 1 (per-message) to match the per-message confirm behavior of other drivers. BATCH_CONFIRM uses batch size 256 like the other drivers.

- [ ] **Step 3: Modify consumeMessages() to respect scenario mode**

Replace `consumeMessages()`:

```php
public function consumeMessages(int $count): void
{
    if ($this->pool === null) {
        throw new RuntimeException('Driver not set up');
    }

    $this->consumer = $this->pool->consumer('default');

    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;

    $consumed = 0;
    $consecutiveNulls = 0;
    while ($consumed < $count) {
        $delivery = $this->consumer->tryNext();
        if ($delivery === null) {
            $delivery = $this->consumer->next(1000);
            if ($delivery === null) {
                $consecutiveNulls++;
                if ($consecutiveNulls >= 3) {
                    break;
                }
                continue;
            }
        }
        $consecutiveNulls = 0;

        $payload = $delivery->payload();
        if (strlen($payload) >= 8) {
            $ts = unpack('P', substr($payload, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }

        if (!$autoAck) {
            $delivery->ack();
        }
        $consumed++;
    }
}
```

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/RabbitRsDriver.php
```

Expected: `No syntax errors detected`

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Drivers/RabbitRsDriver.php
git commit -m "feat(bench): add scenario-aware publish/consume to RabbitRsDriver"
```

---

## Task 7: Implement scenario decorators with real behavior

**Files:**
- Modify: `benchmarks/src/Scenarios/FireAndForgetBenchmark.php`
- Modify: `benchmarks/src/Scenarios/BatchConfirmBenchmark.php`
- Modify: `benchmarks/src/Scenarios/AutoAckBenchmark.php`

**Interfaces:**
- Consumes: `Bench\ScenarioMode` constants, `AbstractBenchmark::setScenarioMode()`
- Produces: Scenario decorators that set the scenario mode on the wrapped driver before running

- [ ] **Step 1: Implement FireAndForgetBenchmark**

Replace entire file content:

```php
<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class FireAndForgetBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::FIRE_AND_FORGET);
    }

    public function getName(): string { return $this->driver->getName() . ' (fire-and-forget)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
}
```

- [ ] **Step 2: Implement BatchConfirmBenchmark**

Replace entire file content:

```php
<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class BatchConfirmBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::BATCH_CONFIRM);
    }

    public function getName(): string { return $this->driver->getName() . ' (batch-confirm)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
}
```

- [ ] **Step 3: Implement AutoAckBenchmark**

Replace entire file content:

```php
<?php

declare(strict_types=1);

namespace Bench\Scenarios;

use Bench\AbstractBenchmark;
use Bench\ScenarioMode;

class AutoAckBenchmark extends AbstractBenchmark
{
    public function __construct(private readonly AbstractBenchmark $driver)
    {
        $driver->setScenarioMode(ScenarioMode::AUTO_ACK);
    }

    public function getName(): string { return $this->driver->getName() . ' (auto-ack)'; }
    public function setUp(): void { $this->driver->setUp(); }
    public function tearDown(): void { $this->driver->tearDown(); }
    public function publishMessages(int $count): void { $this->driver->publishMessages($count); }
    public function consumeMessages(int $count): void { $this->driver->consumeMessages($count); }
}
```

- [ ] **Step 4: Verify syntax for all three**

```bash
php -d xdebug.mode=off -l benchmarks/src/Scenarios/FireAndForgetBenchmark.php
php -d xdebug.mode=off -l benchmarks/src/Scenarios/BatchConfirmBenchmark.php
php -d xdebug.mode=off -l benchmarks/src/Scenarios/AutoAckBenchmark.php
```

Expected: `No syntax errors detected` for all three

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Scenarios/FireAndForgetBenchmark.php benchmarks/src/Scenarios/BatchConfirmBenchmark.php benchmarks/src/Scenarios/AutoAckBenchmark.php
git commit -m "feat(bench): implement real scenario differentiation in decorators"
```

---

## Task 8: Update runner to pass scenario mode and add budget check

**Files:**
- Modify: `benchmarks/src/run-benchmarks.php`

**Interfaces:**
- Consumes: `Bench\Budget` (existing class), `Bench\ScenarioMode`
- Produces: Runner that sets scenario mode, checks losses, enforces budget, prints comparison table

- [ ] **Step 1: Add budget loading after driver/scenario setup**

After the `$scenarios` array definition (line 59), add:

```php
$budgetPath = __DIR__ . '/../baselines/smoke-budget.json';
$budget = null;
if (is_file($budgetPath)) {
    $budget = new Budget($budgetPath);
}
```

Add `use Bench\Budget;` to the imports at the top.

- [ ] **Step 2: Print losses in the output**

In the `try` block after the `printf` for latency (line 90), add:

```php
$losses = $stats['consume']['losses'] ?? 0;
if ($losses > 0) {
    printf("  WARNING: %d messages lost (published %d, consumed %d)\n",
        $losses, Config::MESSAGE_COUNT, Config::MESSAGE_COUNT - $losses);
}
```

- [ ] **Step 3: Add budget check after each benchmark**

After the `printf` lines and loss warning, add:

```php
if ($budget !== null) {
    $budgetResult = $budget->check($stats['publish'], $stats['consume']);
    echo $budget->formatResult($budgetResult);
}
```

- [ ] **Step 4: Print summary table at the end**

Before writing the JSON results, add a summary table:

```php
echo "\n=== Summary ===\n";
printf("%-30s | %-15s | %-15s | %-10s | %-10s\n", "Scenario/Driver", "Publish msg/s", "Consume msg/s", "p99 (ms)", "Losses");
echo str_repeat('-', 90) . "\n";
foreach ($allResults as $key => $stats) {
    printf("%-30s | %-15.0f | %-15.0f | %-10.2f | %-10d\n",
        $key,
        $stats['publish']['avg_rate'],
        $stats['consume']['avg_rate'],
        $stats['publish']['p99'],
        $stats['consume']['losses'] ?? 0
    );
}
echo "\n";
```

- [ ] **Step 5: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/run-benchmarks.php
```

Expected: `No syntax errors detected`

- [ ] **Step 6: Commit**

```bash
git add benchmarks/src/run-benchmarks.php
git commit -m "feat(bench): add budget enforcement, loss reporting, and summary table"
```

---

## Task 9: Fix Xdebug configuration for benchmarks

**Files:**
- Modify: `benchmarks/run-benchmarks.sh`

**Interfaces:**
- Produces: Runner script that disables Xdebug during benchmark execution

- [ ] **Step 1: Add Xdebug disable flag to PHP invocation**

In `benchmarks/run-benchmarks.sh`, replace the last line:

```bash
php src/run-benchmarks.php "$@"
```

With:

```bash
php -d xdebug.mode=off src/run-benchmarks.php "$@"
```

This disables all Xdebug features at runtime without modifying the ini file. The Xdebug extension will still be loaded (causing the "MUST be loaded as a Zend extension" warning), but all profiling/tracing/stepping features will be disabled, eliminating the performance impact.

- [ ] **Step 2: Fix the docker-compose check**

The current check `docker compose ps --status running | grep -q rabbitmq-benchmark` uses `docker compose` (v2 plugin syntax) which may not be available on all systems. Also, when using the lab cluster, we don't need the standalone container.

Replace the docker check with a connectivity check to the lab:

```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# Check if RabbitMQ is reachable on localhost:5672
if ! curl -s -o /dev/null -u rabbit_rs:rabbit_rs_lab http://localhost:15672/api/overview; then
    echo "RabbitMQ not reachable at localhost:5672."
    echo "Start the lab with: ./scripts/lab-up.sh"
    exit 1
fi

if [ ! -d vendor ]; then
    composer install --no-interaction
fi

php -d xdebug.mode=off src/run-benchmarks.php "$@"
```

- [ ] **Step 3: Commit**

```bash
git add benchmarks/run-benchmarks.sh
git commit -m "fix(bench): disable Xdebug in runner and use lab connectivity check"
```

---

## Task 10: Remove vestigial Driver interface

**Files:**
- Delete: `benchmarks/src/Driver.php`
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php:24` — replace `DriverUnavailableException` with `RuntimeException`

**Interfaces:**
- Produces: Clean codebase without unused `Driver` interface and `DriverUnavailableException`

- [ ] **Step 1: Replace DriverUnavailableException in AmqpExtDriver**

In `benchmarks/src/Drivers/AmqpExtDriver.php`, line 24, change:

```php
throw new DriverUnavailableException('The pecl "amqp" extension is not loaded');
```

To:

```php
throw new \RuntimeException('The pecl "amqp" extension is not loaded');
```

- [ ] **Step 2: Delete Driver.php**

```bash
rm benchmarks/src/Driver.php
```

- [ ] **Step 3: Verify no other references to Driver interface or DriverUnavailableException**

```bash
grep -rn "DriverUnavailableException\|Bench\\\\Driver\b\|use Bench\\\\Driver" benchmarks/src/ --include="*.php"
```

Expected: no matches (only `Bench\Drivers\*` namespace references, which are different)

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqpExtDriver.php
```

Expected: `No syntax errors detected`

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Drivers/AmqpExtDriver.php benchmarks/src/Driver.php
git commit -m "refactor(bench): remove vestigial Driver interface and DriverUnavailableException"
```

---

## Task 11: Fix docker-compose.yml credentials and update README

**Files:**
- Modify: `benchmarks/docker-compose.yml`
- Modify: `benchmarks/README.md`

**Interfaces:**
- Produces: Consistent docker-compose credentials and accurate documentation

- [ ] **Step 1: Fix docker-compose.yml credentials**

In `benchmarks/docker-compose.yml`, change the environment section:

```yaml
    environment:
      RABBITMQ_DEFAULT_USER: rabbit_rs
      RABBITMQ_DEFAULT_PASS: rabbit_rs_lab
      RABBITMQ_DEFAULT_VHOST: /
```

- [ ] **Step 2: Rewrite README.md to match actual structure**

Replace entire `benchmarks/README.md` content:

```markdown
# Rabbit RS Benchmarks

Standalone PHP benchmark suite for measuring rabbit-rs throughput, latency, and reliability against other PHP RabbitMQ drivers.

## Quick start

### Prerequisites

- PHP 8.4 or later
- RabbitMQ broker at `127.0.0.1:5672` (start the lab with `./scripts/lab-up.sh`)
- `rabbit_rs` extension loaded (for rabbit-rs driver)
- `composer install --working-dir=benchmarks` (for amqplib, bunny, and Laravel benchmarks)
- pecl `amqp` extension (for amqp-ext driver, optional)

### Run all benchmarks

```bash
./benchmarks/run-benchmarks.sh
```

This runs all 3 scenarios x all available drivers (up to 12 combinations).

### Run specific driver or scenario

```bash
./benchmarks/run-benchmarks.sh --driver=amqplib
./benchmarks/run-benchmarks.sh --scenario=fire-and-forget
./benchmarks/run-benchmarks.sh --driver=rabbit-rs --scenario=batch-confirm
```

### Available drivers

| Driver | Requires | Description |
|--------|----------|-------------|
| `rabbit-rs` | ext-rabbit_rs | Native Rust-backed PHP extension |
| `amqplib` | composer install | Pure PHP AMQP client (php-amqplib) |
| `amqp-ext` | pecl amqp | C-based AMQP extension |
| `bunny` | composer install | Async PHP AMQP client (bunny/bunny) |

Drivers are auto-detected based on available extensions and classes.

### Scenarios

| Scenario | Publish | Consume |
|----------|---------|--------|
| `fire-and-forget` | No confirms, no mandatory flag | `no_ack=true` (auto-ack by broker) |
| `batch-confirm` | Batched confirms (every 256 msgs), mandatory flag | Manual ACK |
| `auto-ack` | Per-message confirms, mandatory flag | `no_ack=true` (auto-ack by broker) |

Note: The `rabbit_rs` extension always uses confirms internally. For `fire-and-forget`, a 100ms timeout approximates fire-and-forget behavior.

### Budget system

The smoke budget (`baselines/smoke-budget.json`) checks:

| Metric | Check |
|--------|-------|
| `publish_throughput_min` | `actual >= budget` |
| `consume_throughput_min` | `actual >= budget` |
| `publish_p99_max_ms` | `actual <= budget` |
| `consume_p99_max_ms` | `actual <= budget` |
| `losses_max` | `actual == 0` |

### Configuration

All benchmark parameters are in `src/Config.php`:
- 10,000 messages per round, 10 rounds (+ 1 warmup)
- 256-byte payload
- RabbitMQ: `127.0.0.1:5672`, user `rabbit_rs`, vhost `/`

### Latency measurement

End-to-end publish-to-consume latency is measured by embedding `hrtime(true)` (nanoseconds) in the first 8 bytes of the message payload (packed as 64-bit unsigned int). On consume, the timestamp is unpacked and the delta is computed.

### Output

Results are printed to stdout and written to `results/benchmark-results.json`.

### Directory structure

```
benchmarks/
├── README.md
├── composer.json
├── docker-compose.yml       # Standalone RabbitMQ (for CI, uses rabbit_rs/rabbit_rs_lab)
├── run-benchmarks.sh         # Shell wrapper
├── baselines/
│   └── smoke-budget.json     # Budget thresholds
├── results/                  # Output directory (gitignored)
├── src/
│   ├── run-benchmarks.php    # Main runner
│   ├── AbstractBenchmark.php # Base class: measurement loop, stats
│   ├── Config.php            # Static config constants
│   ├── ScenarioMode.php       # Scenario mode constants
│   ├── Budget.php             # Budget checking
│   ├── Drivers/
│   │   ├── AmqplibDriver.php
│   │   ├── AmqpExtDriver.php
│   │   ├── BunnyDriver.php
│   │   └── RabbitRsDriver.php
│   └── Scenarios/
│       ├── FireAndForgetBenchmark.php
│       ├── BatchConfirmBenchmark.php
│       └── AutoAckBenchmark.php
└── laravel/
    ├── LaravelCompareBenchmark.php
    └── LaravelSmokeBenchmark.php
```

### Rust microbenchmarks

Criterion benchmarks for subsystem-level performance (if available):

```bash
cargo bench -p rabbit-rs-core
```
```

- [ ] **Step 3: Commit**

```bash
git add benchmarks/docker-compose.yml benchmarks/README.md
git commit -m "fix(bench): align docker-compose credentials and rewrite README"
```

---

## Task 12: Install dependencies and build extensions

**Files:**
- No file changes — environment setup only

- [ ] **Step 1: Run composer install in benchmarks**

```bash
cd <worktree-root>/benchmarks && composer install --no-interaction
```

Expected: `vendor/` directory created with php-amqplib, bunny, illuminate packages.

- [ ] **Step 2: Build and install the rabbit_rs extension**

```bash
cd <worktree-root> && ./scripts/install.sh --release --yes
```

Expected: Extension compiled and installed. Verify with:

```bash
php -d xdebug.mode=off -m | grep rabbit_rs
```

Expected: `rabbit_rs` in the module list.

- [ ] **Step 3: Install pecl amqp extension**

```bash
pecl install amqp
```

If a config file is not automatically created, add `extension=amqp` to PHP conf.d:

```bash
echo "extension=amqp" > /opt/homebrew/etc/php/8.4/conf.d/ext-amqp.ini
```

Verify:

```bash
php -d xdebug.mode=off -m | grep amqp
```

Expected: `amqp` in the module list.

- [ ] **Step 4: Verify all 4 drivers are detected**

```bash
cd <worktree-root>/benchmarks && php -d xdebug.mode=off -r '
spl_autoload_register(static function (string $class): void {
    $prefixes = [
        "Bench\\Drivers\\" => __DIR__ . "/src/Drivers/",
        "Bench\\Scenarios\\" => __DIR__ . "/src/Scenarios/",
        "Bench\\" => __DIR__ . "/src/",
    ];
    foreach ($prefixes as $prefix => $base) {
        if (str_starts_with($class, $prefix)) {
            $relative = substr($class, strlen($prefix));
            $file = $base . str_replace("\\", "/", $relative) . ".php";
            if (is_file($file)) { require $file; }
            return;
        }
    }
});
if (is_file(__DIR__ . "/vendor/autoload.php")) { require __DIR__ . "/vendor/autoload.php"; }

echo "rabbit_rs: " . (extension_loaded("rabbit_rs") ? "loaded" : "NOT loaded") . "\n";
echo "amqplib: " . (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class) ? "detected" : "NOT detected") . "\n";
echo "amqp-ext: " . (extension_loaded("amqp") ? "loaded" : "NOT loaded") . "\n";
echo "bunny: " . (class_exists(\Bunny\Client::class) ? "detected" : "NOT detected") . "\n";
'
```

Expected: all 4 detected/loaded.

- [ ] **Step 5: No commit needed (environment setup only)**

---

## Task 13: Smoke test — single driver, single scenario

**Files:**
- No file changes — verification only

- [ ] **Step 1: Run smoke test with amqplib driver**

```bash
cd <worktree-root>/benchmarks && php -d xdebug.mode=off src/run-benchmarks.php --driver=amqplib --scenario=fire-and-forget
```

Expected: 
- Publish and consume rates printed
- Latency percentiles printed
- No errors
- Results written to `results/benchmark-results.json`

- [ ] **Step 2: Run smoke test with rabbit-rs driver**

```bash
php -d xdebug.mode=off src/run-benchmarks.php --driver=rabbit-rs --scenario=batch-confirm
```

Expected: same success criteria as Step 1.

- [ ] **Step 3: If any driver fails, debug the specific failure**

Common issues:
- Connection refused: verify RabbitMQ is running on localhost:5672
- Auth failure: verify `rabbit_rs`/`rabbit_rs_lab` credentials work (test with `curl -u rabbit_rs:rabbit_rs_lab http://localhost:15672/api/overview`)
- Extension not loaded: verify `php -m | grep rabbit_rs`
- Class not found: verify `composer install` completed

- [ ] **Step 4: No commit needed (verification only)**

---

## Task 14: Full benchmark run — all drivers, all scenarios

**Files:**
- No file changes — verification only

- [ ] **Step 1: Run the full benchmark suite**

```bash
cd <worktree-root>/benchmarks && php -d xdebug.mode=off src/run-benchmarks.php
```

Expected:
- 12 combinations (3 scenarios x 4 drivers) attempted
- Summary table printed at the end
- Budget checks printed for each combination
- JSON results written to `results/benchmark-results.json`
- No fatal errors (some drivers may SKIP if env not ready)

- [ ] **Step 2: Verify results JSON is non-empty**

```bash
php -d xdebug.mode=off -r 'echo count(json_decode(file_get_contents("results/benchmark-results.json"), true)) . " results\n";'
```

Expected: a number > 0 (ideally 12 if all drivers work).

- [ ] **Step 3: No commit needed (verification only)**

---

## Task 15: Optimize AbstractBenchmark — message template pre-generation

**Files:**
- Modify: `benchmarks/src/AbstractBenchmark.php`

**Interfaces:**
- Produces: `createMessage()` that caches the JSON template instead of rebuilding it per message

- [ ] **Step 1: Pre-generate the message payload template**

In `AbstractBenchmark.php`, replace the `createMessage()` method:

```php
private ?string $messageTemplate = null;

protected function createMessage(string $body): string
{
    if ($this->messageTemplate === null) {
        $this->messageTemplate = json_encode([
            'id' => '%s',
            'timestamp' => '%f',
            'data' => '%s',
            'payload' => str_repeat('x', Config::MESSAGE_PAYLOAD_BYTES),
        ]);
    }
    return sprintf($this->messageTemplate, uniqid('', true), microtime(true), $body);
}
```

Wait — `sprintf` with `%f` for microtime could have precision issues. Let's use a simpler approach: pre-generate the static payload portion and concatenate the dynamic parts:

```php
private ?string $payloadPadding = null;

protected function createMessage(string $body): string
{
    if ($this->payloadPadding === null) {
        $this->payloadPadding = str_repeat('x', Config::MESSAGE_PAYLOAD_BYTES);
    }
    return json_encode([
        'id' => uniqid('', true),
        'timestamp' => microtime(true),
        'data' => $body,
        'payload' => $this->payloadPadding,
    ]);
}
```

This avoids `str_repeat` being called 10,000+ times per round (the JSON encoding still happens per message, but the expensive `str_repeat` is cached).

- [ ] **Step 2: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/AbstractBenchmark.php
```

- [ ] **Step 3: Commit**

```bash
git add benchmarks/src/AbstractBenchmark.php
git commit -m "perf(bench): cache payload padding to avoid repeated str_repeat"
```

---

## Task 16: Optimize UUID generation — use counter instead of random bytes

**Files:**
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php`
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php`
- Modify: `benchmarks/src/Drivers/BunnyDriver.php`
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php`

**Interfaces:**
- Produces: Drivers that use a monotonic counter for message_id instead of `random_bytes(16)` + `vsprintf`

- [ ] **Step 1: Add a shared UUID counter method to AbstractBenchmark**

In `AbstractBenchmark.php`, add:

```php
private int $messageCounter = 0;

protected function uuid(): string
{
    return 'bench-' . getmypid() . '-' . (++$this->messageCounter);
}
```

- [ ] **Step 2: Remove the private `uuid()` method from all 4 drivers**

In each driver file, remove the `private function uuid(): string` method (the last method in each file, ~10 lines).

The drivers will now use the inherited `AbstractBenchmark::uuid()` method.

- [ ] **Step 3: Verify syntax for all modified files**

```bash
php -d xdebug.mode=off -l benchmarks/src/AbstractBenchmark.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqplibDriver.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqpExtDriver.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/BunnyDriver.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/RabbitRsDriver.php
```

Expected: `No syntax errors detected` for all.

- [ ] **Step 4: Commit**

```bash
git add benchmarks/src/AbstractBenchmark.php benchmarks/src/Drivers/
git commit -m "perf(bench): use monotonic counter for message_id instead of random_bytes"
```

---

## Task 17: Align prefetch count across drivers

**Files:**
- Modify: `benchmarks/src/Drivers/AmqplibDriver.php:49`
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php:58`
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php:44`

**Interfaces:**
- Consumes: `Config::PREFETCH_COUNT` (value: 500)
- Produces: All drivers using the same configurable prefetch count

- [ ] **Step 1: Fix AmqplibDriver prefetch**

In `AmqplibDriver.php`, line 49, change:

```php
$this->consChannel->basic_qos(0, 16, false);
```

To:

```php
$this->consChannel->basic_qos(0, Config::PREFETCH_COUNT, false);
```

- [ ] **Step 2: Fix AmqpExtDriver prefetch**

In `AmqpExtDriver.php`, line 58, change:

```php
$this->channel->setPrefetchCount(16);
```

To:

```php
$this->channel->setPrefetchCount(Config::PREFETCH_COUNT);
```

- [ ] **Step 3: Fix RabbitRsDriver prefetch**

In `RabbitRsDriver.php`, line 44, change:

```php
'prefetch' => 64,
```

To:

```php
'prefetch' => Config::PREFETCH_COUNT,
```

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqplibDriver.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/AmqpExtDriver.php
php -d xdebug.mode=off -l benchmarks/src/Drivers/RabbitRsDriver.php
```

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Drivers/AmqplibDriver.php benchmarks/src/Drivers/AmqpExtDriver.php benchmarks/src/Drivers/RabbitRsDriver.php
git commit -m "fix(bench): align prefetch count to Config::PREFETCH_COUNT across all drivers"
```

---

## Task 18: Fix Budget metric key mapping

**Files:**
- Modify: `benchmarks/src/Budget.php`

**Interfaces:**
- Consumes: Stats from `AbstractBenchmark::runBenchmark()` which uses keys like `avg_rate`, `p99`, `losses`
- Produces: Budget that correctly maps its keys to the actual stats structure

The current `Budget::extractMetric()` looks for `$publishMetrics['throughput']` and `$consumeMetrics['throughput']`, but the actual stats use `avg_rate`. It also looks for `$consumeMetrics['p99']` but p99 is under `publish`, not `consume`.

- [ ] **Step 1: Fix extractMetric to match actual stats keys**

In `Budget.php`, replace the `extractMetric()` method:

```php
private function extractMetric(string $key, array $publishMetrics, array $consumeMetrics): ?float
{
    return match ($key) {
        'publish_throughput_min' => isset($publishMetrics['avg_rate']) ? (float) $publishMetrics['avg_rate'] : null,
        'consume_throughput_min' => isset($consumeMetrics['avg_rate']) ? (float) $consumeMetrics['avg_rate'] : null,
        'publish_p99_max_ms' => isset($publishMetrics['p99']) ? (float) $publishMetrics['p99'] : null,
        'consume_p99_max_ms' => isset($consumeMetrics['p95']) ? (float) $consumeMetrics['p95'] : null,
        'losses_max' => isset($consumeMetrics['losses']) ? (float) $consumeMetrics['losses'] : null,
        default => null,
    };
}
```

Note: the consume stats don't have a `p99` key — they have `p50` and `p95`. Using `p95` for `consume_p99_max_ms` is the closest available. Alternatively, add `p99` to the consume stats in `AbstractBenchmark::calculateStats()`. Let's do that instead for accuracy.

Actually, let's add p99 to the consume stats in `calculateStats()` first, then use it in Budget.

- [ ] **Step 2: Add p99 to consume stats in AbstractBenchmark**

In `AbstractBenchmark.php`, in `calculateStats()`, under the `'consume'` array, after `'p95'`, add:

```php
'p99' => $avg($get('p99')),
```

Wait — the per-round results already compute `p99` (line 48: `'p99' => $this->percentile(0.99),`). It's just not included in the consume stats aggregation. Let's add it:

In the `'consume'` array in `calculateStats()`, after `'p95' => $avg($get('p95')),` add:

```php
'p99' => $avg($get('p99')),
```

- [ ] **Step 3: Now fix Budget to use consume p99**

Update the `extractMetric()` in Budget:

```php
'consume_p99_max_ms' => isset($consumeMetrics['p99']) ? (float) $consumeMetrics['p99'] : null,
```

- [ ] **Step 4: Verify syntax**

```bash
php -d xdebug.mode=off -l benchmarks/src/Budget.php
php -d xdebug.mode=off -l benchmarks/src/AbstractBenchmark.php
```

- [ ] **Step 5: Commit**

```bash
git add benchmarks/src/Budget.php benchmarks/src/AbstractBenchmark.php
git commit -m "fix(bench): align budget metric keys with actual stats structure, add consume p99"
```

---

## Task 19: Final verification — full quality gate

**Files:**
- No file changes — verification only

- [ ] **Step 1: Run PHP lint on all modified files**

```bash
cd <worktree-root>/benchmarks && find src/ -name "*.php" -exec php -d xdebug.mode=off -l {} \; 2>&1 | grep -v "No syntax errors"
```

Expected: no output (all files pass lint).

- [ ] **Step 2: Run the full benchmark suite**

```bash
php -d xdebug.mode=off src/run-benchmarks.php
```

Expected:
- All 12 combinations run (or SKIP gracefully for missing drivers)
- Summary table printed
- Budget checks printed
- JSON written to `results/benchmark-results.json`

- [ ] **Step 3: Run the Rust quality gate to verify no regressions**

```bash
cd <worktree-root> && ./scripts/check.sh
```

Expected: all checks pass (fmt + clippy + test + composer validate).

- [ ] **Step 4: No commit needed (verification only)**
