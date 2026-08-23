# Horizon Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Laravel Horizon support so Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs, with RabbitMQ as transport and Redis for Horizon's job tracking.

**Architecture:** Two new subclass classes (`Horizon\RabbitMqQueue` and `Horizon\RabbitMqJob`) dispatch Horizon's `JobPending`, `JobPushed`, `JobReserved`, and `JobDeleted` events. A `worker` config option (`default` or `horizon`) selects the variant at connector time. No changes to Rust core or the PHP extension.

**Tech Stack:** PHP 8.4+, Laravel 12/13, Laravel Horizon (optional), Pest 4, Orchestra Testbench 10/11.

## Global Constraints

- PHP 8.4+ and Laravel 12/13 only.
- `laravel/horizon` is an optional dependency — never require it in `composer.json`.
- Classes in `src/Horizon/` import from `Laravel\Horizon\*` and must only be loaded when Horizon is installed.
- Test fakes in `tests/bootstrap.php` must include stub classes for `Laravel\Horizon\Events\*` and `Laravel\Horizon\JobPayload` so unit tests run without the Horizon package installed.
- No changes to Rust core (`crates/rabbit-rs-core/`) or the PHP extension (`crates/rabbit-rs-php/`).
- Pest tests, not PHPUnit. Unit/Feature tests run without the extension loaded.
- Follow existing code conventions: `declare(strict_types=1)`, `final` where appropriate, readonly properties.

---

## File Structure

| File | Responsibility | Action |
|------|---------------|--------|
| `packages/laravel-queue/src/RabbitMqQueue.php` | Remove `final` keyword to allow subclassing | Modify line 24 |
| `packages/laravel-queue/src/Jobs/RabbitMqJob.php` | Remove `final` keyword to allow subclassing | Modify line 13 |
| `packages/laravel-queue/src/Horizon/RabbitMqQueue.php` | Horizon queue subclass dispatching events | Create |
| `packages/laravel-queue/src/Horizon/RabbitMqJob.php` | Horizon job subclass calling deleteReserved | Create |
| `packages/laravel-queue/src/Connectors/RabbitMqConnector.php` | Dynamic class resolution by `worker` config | Modify lines 36-63 |
| `packages/laravel-queue/config/rabbit-rs.php` | Add `worker` config key | Modify (after line 331) |
| `packages/laravel-queue/composer.json` | Add `laravel/horizon` to `suggest` | Modify |
| `packages/laravel-queue/tests/bootstrap.php` | Add Horizon event + JobPayload fakes | Modify |
| `packages/laravel-queue/tests/Unit/HorizonRabbitMqQueueTest.php` | Unit tests for Horizon queue | Create |
| `packages/laravel-queue/tests/Unit/HorizonRabbitMqJobTest.php` | Unit tests for Horizon job | Create |
| `packages/laravel-queue/tests/Unit/RabbitMqConnectorHorizonTest.php` | Unit tests for connector worker resolution | Create |

---

### Task 1: Remove `final` from `RabbitMqQueue` and `RabbitMqJob`

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:24`
- Modify: `packages/laravel-queue/src/Jobs/RabbitMqJob.php:13`

**Interfaces:**
- Produces: `RabbitMqQueue` (non-final, extendable) and `RabbitMqJob` (non-final, extendable)

- [ ] **Step 1: Remove `final` from `RabbitMqQueue`**

In `packages/laravel-queue/src/RabbitMqQueue.php`, line 24, change:

```php
final class RabbitMqQueue extends Queue implements QueueContract
```

to:

```php
class RabbitMqQueue extends Queue implements QueueContract
```

- [ ] **Step 2: Remove `final` from `RabbitMqJob`**

In `packages/laravel-queue/src/Jobs/RabbitMqJob.php`, line 13, change:

```php
final class RabbitMqJob extends Job implements JobContract
```

to:

```php
class RabbitMqJob extends Job implements JobContract
```

- [ ] **Step 3: Run existing tests to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS — all existing unit tests still pass (removing `final` is backward-compatible)

- [ ] **Step 4: Commit**

```bash
git add packages/laravel-queue/src/RabbitMqQueue.php packages/laravel-queue/src/Jobs/RabbitMqJob.php
git commit -m "refactor(laravel): remove final from RabbitMqQueue and RabbitMqJob for Horizon subclassing"
```

---

### Task 2: Add Horizon event and JobPayload fakes to test bootstrap

**Files:**
- Modify: `packages/laravel-queue/tests/bootstrap.php`

**Interfaces:**
- Produces: stub classes `Laravel\Horizon\Events\RedisEvent`, `JobPending`, `JobPushed`, `JobReserved`, `JobDeleted`, and `Laravel\Horizon\JobPayload` for unit tests without the Horizon package installed.

- [ ] **Step 1: Add Horizon fakes to bootstrap.php**

After the `Laravel\Octane\Events` namespace block (line 23) and before the `Goopil\RabbitRs` namespace block (line 25), add:

```php
namespace Laravel\Horizon {
    if (! class_exists(JobPayload::class, false)) {
        class JobPayload
        {
            public string $value;

            public array $decoded;

            public function __construct(string $value)
            {
                $this->value = $value;
                $this->decoded = json_decode($value, true) ?: [];
            }

            public function prepare(mixed $job = null): self
            {
                $this->decoded['type'] = 'job';
                $this->decoded['tags'] = $this->decoded['tags'] ?? [];
                $this->decoded['silenced'] = false;
                $this->decoded['pushedAt'] = '1234567890.1234';
                $this->value = json_encode($this->decoded);

                return $this;
            }

            public function id(): string
            {
                return $this->decoded['uuid'] ?? $this->decoded['id'] ?? '';
            }
        }
    }
}

namespace Laravel\Horizon\Events {
    if (! class_exists(RedisEvent::class, false)) {
        class RedisEvent
        {
            public ?string $connectionName = null;

            public ?string $queue = null;

            public JobPayload $payload;

            public function __construct(string $payload)
            {
                $this->payload = new JobPayload($payload);
            }

            public function connection(string $connectionName): self
            {
                $this->connectionName = $connectionName;

                return $this;
            }

            public function queue(string $queue): self
            {
                $this->queue = $queue;

                return $this;
            }
        }
    }

    if (! class_exists(JobPending::class, false)) {
        class JobPending extends RedisEvent {}
    }

    if (! class_exists(JobPushed::class, false)) {
        class JobPushed extends RedisEvent {}
    }

    if (! class_exists(JobReserved::class, false)) {
        class JobReserved extends RedisEvent {}
    }

    if (! class_exists(JobDeleted::class, false)) {
        class JobDeleted extends RedisEvent
        {
            public mixed $job;

            public function __construct(mixed $job, string $payload)
            {
                parent::__construct($payload);
                $this->job = $job;
            }
        }
    }
}
```

- [ ] **Step 2: Run existing tests to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS — all existing unit tests still pass

- [ ] **Step 3: Commit**

```bash
git add packages/laravel-queue/tests/bootstrap.php
git commit -m "test: add Horizon event and JobPayload fakes to test bootstrap"
```

---

### Task 3: Create `Horizon\RabbitMqQueue` with event dispatching

**Files:**
- Create: `packages/laravel-queue/src/Horizon/RabbitMqQueue.php`

**Interfaces:**
- Consumes: `Goopil\RabbitRs\Laravel\RabbitMqQueue` (parent class, non-final after Task 1)
- Consumes: `Laravel\Horizon\Events\JobPending`, `JobPushed`, `JobReserved`, `JobDeleted`
- Consumes: `Laravel\Horizon\JobPayload`
- Produces: `Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue` — a queue class that dispatches Horizon events on push, later, pop, and delete

- [ ] **Step 1: Write the failing test**

Create `packages/laravel-queue/tests/Unit/HorizonRabbitMqQueueTest.php`:

```php
<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqJob as HorizonRabbitMqJob;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Illuminate\Contracts\Events\Dispatcher;
use Laravel\Horizon\Events\JobDeleted;
use Laravel\Horizon\Events\JobPending;
use Laravel\Horizon\Events\JobPushed;
use Laravel\Horizon\Events\JobReserved;

function horizonQueue(Pool $pool = new Pool()): HorizonRabbitMqQueue
{
    $queue = new HorizonRabbitMqQueue($pool, [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'jobs',
            'routing_key' => '{queue}',
        ],
    ], 'default');
    $queue->setContainer(app());
    $queue->setConnectionName('rabbit-rs');

    return $queue;
}

function recordedEvents(Dispatcher $dispatcher): array
{
    $events = [];
    $dispatcher->listen(static function (object $event) use (&$events): void {
        $events[] = $event;
    });

    return $events;
}

beforeEach(function (): void {
    $this->events = $this->createMock(Dispatcher::class);
    $this->app->instance(Dispatcher::class, $this->events);
});

it('dispatches JobPending then JobPushed on push', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void { $events[] = $event; }
    );

    $queue->push('TestJob', ['key' => 'value'], 'orders');

    expect($events)->toHaveCount(2)
        ->and($events[0])->toBeInstanceOf(JobPending::class)
        ->and($events[1])->toBeInstanceOf(JobPushed::class)
        ->and($events[0]->queue)->toBe('orders')
        ->and($events[1]->queue)->toBe('orders')
        ->and($events[0]->connectionName)->toBe('rabbit-rs')
        ->and($events[1]->connectionName)->toBe('rabbit-rs');

    $payload = json_decode($events[0]->payload->value, true);
    expect($payload)->toHaveKey('type')
        ->and($payload)->toHaveKey('tags')
        ->and($payload)->toHaveKey('pushedAt');
});

it('dispatches JobPending then JobPushed on later', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void { $events[] = $event; }
    );

    $queue->later(10, 'TestJob', ['key' => 'value'], 'orders');

    expect($events)->toHaveCount(2)
        ->and($events[0])->toBeInstanceOf(JobPending::class)
        ->and($events[1])->toBeInstanceOf(JobPushed::class);
});

it('dispatches JobReserved on pop when a job is returned', function (): void {
    $pool = new Pool();
    $delivery = new Goopil\RabbitRs\Delivery(
        json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob', 'data' => []]),
        ['message_id' => 'test-uuid', 'subscription' => 'default', 'attempts' => 1, 'state' => 'pending', 'headers' => []],
    );
    $pool->pushDelivery('default', $delivery);

    $queue = horizonQueue($pool);
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void { $events[] = $event; }
    );

    $job = $queue->pop('default');

    expect($job)->toBeInstanceOf(HorizonRabbitMqJob::class)
        ->and($events)->toHaveCount(1)
        ->and($events[0])->toBeInstanceOf(JobReserved::class)
        ->and($events[0]->queue)->toBe('default')
        ->and($events[0]->connectionName)->toBe('rabbit-rs');
});

it('does not dispatch any event on pop when no job is available', function (): void {
    $queue = horizonQueue(new Pool());
    $dispatchCount = 0;
    $this->events->method('dispatch')->willReturnCallback(
        static function () use (&$dispatchCount): void { $dispatchCount++; }
    );

    $result = $queue->pop('default');

    expect($result)->toBeNull()
        ->and($dispatchCount)->toBe(0);
});

it('dispatches JobDeleted on deleteReserved', function (): void {
    $queue = horizonQueue();
    $events = [];
    $this->events->method('dispatch')->willReturnCallback(
        static function (object $event) use (&$events): void { $events[] = $event; }
    );

    $job = $this->createMock(RabbitMqJob::class);
    $job->method('getRawBody')->willReturn(json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob']));

    $queue->deleteReserved('orders', $job);

    expect($events)->toHaveCount(1)
        ->and($events[0])->toBeInstanceOf(JobDeleted::class)
        ->and($events[0]->queue)->toBe('orders')
        ->and($events[0]->connectionName)->toBe('rabbit-rs');
});

it('marshalJob creates a HorizonRabbitMqJob', function (): void {
    $queue = horizonQueue();
    $delivery = new Goopil\RabbitRs\Delivery(
        json_encode(['uuid' => 'test-uuid', 'job' => 'TestJob', 'data' => []]),
        ['message_id' => 'test-uuid', 'subscription' => 'default', 'attempts' => 1, 'state' => 'pending', 'headers' => []],
    );

    $job = $queue->marshalJob($delivery, 'orders');

    expect($job)->toBeInstanceOf(HorizonRabbitMqJob::class)
        ->and($job)->toBeInstanceOf(RabbitMqJob::class)
        ->and($job->getQueue())->toBe('orders');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/HorizonRabbitMqQueueTest.php`
Expected: FAIL — `HorizonRabbitMqQueue` class not found

- [ ] **Step 3: Create `Horizon\RabbitMqQueue`**

Create `packages/laravel-queue/src/Horizon/RabbitMqQueue.php`:

```php
<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Horizon;

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Illuminate\Contracts\Events\Dispatcher;
use Laravel\Horizon\Events\JobDeleted;
use Laravel\Horizon\Events\JobPending;
use Laravel\Horizon\Events\JobPushed;
use Laravel\Horizon\Events\JobReserved;
use Laravel\Horizon\JobPayload;

class RabbitMqQueue extends RabbitMqQueue
{
    protected mixed $lastPushed = null;

    public function push($job, $data = '', $queue = null)
    {
        $this->lastPushed = $job;

        return parent::push($job, $data, $queue);
    }

    public function pushRaw($payload, $queue = null, array $options = [])
    {
        $payload = (new JobPayload($payload))->prepare($this->lastPushed ?? null)->value;

        $this->event($this->queueName($queue), new JobPending($payload));

        return tap(parent::pushRaw($payload, $queue, $options), function (string $messageId) use ($queue, $payload): void {
            $this->event($this->queueName($queue), new JobPushed($payload));
        });
    }

    public function later($delay, $job, $data = '', $queue = null)
    {
        $payload = (new JobPayload($this->createPayload($job, $this->queueName($queue), $data)))->prepare($job)->value;

        $this->event($this->queueName($queue), new JobPending($payload));

        return tap(parent::laterRawFromPayload($delay, $payload, $queue), function () use ($queue, $payload): void {
            $this->event($this->queueName($queue), new JobPushed($payload));
        });
    }

    public function pop($queue = null, $index = 0)
    {
        return tap(parent::pop($queue, $index), function (mixed $result) use ($queue): void {
            if ($result instanceof RabbitMqJob) {
                $this->event($this->queueName($queue), new JobReserved($result->getRawBody()));
            }
        });
    }

    public function marshalJob(Delivery $delivery, $queue = null): RabbitMqJob
    {
        return new HorizonRabbitMqJob(
            $this->container,
            $delivery,
            $this->connectionName,
            $this->queueName($queue),
            $this,
        );
    }

    public function deleteReserved(string $queue, RabbitMqJob $job): void
    {
        $this->event($queue, new JobDeleted($job, $job->getRawBody()));
    }

    protected function event(string $queue, object $event): void
    {
        if ($this->container && $this->container->bound(Dispatcher::class)) {
            $this->container->make(Dispatcher::class)->dispatch(
                $event->connection($this->connectionName)->queue($queue)
            );
        }
    }
}
```

- [ ] **Step 4: Add `laterRawFromPayload` helper to base `RabbitMqQueue`**

The `Horizon\RabbitMqQueue::later()` override needs to prepare the payload before passing it to the parent. The parent's `later()` calls `enqueueUsing()` which creates the payload from the job — but we already have a prepared payload. Add a `protected` method to the base class that accepts a raw payload:

In `packages/laravel-queue/src/RabbitMqQueue.php`, add after the `later()` method (around line 181):

```php
    /**
     * Publish a raw payload with a delay, bypassing enqueueUsing.
     * Used by Horizon subclass to dispatch with an already-prepared payload.
     */
    protected function laterRawFromPayload($delay, string $payload, $queue = null): string
    {
        return $this->publish(
            $payload,
            $queue,
            ['content_type' => 'application/json'],
            $this->delayMilliseconds($delay),
        );
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/HorizonRabbitMqQueueTest.php`
Expected: PASS

- [ ] **Step 6: Run full unit suite to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS — all existing unit tests still pass

- [ ] **Step 7: Commit**

```bash
git add packages/laravel-queue/src/Horizon/RabbitMqQueue.php packages/laravel-queue/src/RabbitMqQueue.php packages/laravel-queue/tests/Unit/HorizonRabbitMqQueueTest.php
git commit -m "feat(horizon): add Horizon\RabbitMqQueue with event dispatching"
```

---

### Task 4: Create `Horizon\RabbitMqJob` with deleteReserved call

**Files:**
- Create: `packages/laravel-queue/src/Horizon/RabbitMqJob.php`

**Interfaces:**
- Consumes: `Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob` (parent class, non-final after Task 1)
- Consumes: `Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue` (from Task 3) — calls `deleteReserved()`
- Produces: `Goopil\RabbitRs\Laravel\Horizon\RabbitMqJob` — a job that calls `deleteReserved()` on the queue after delete

- [ ] **Step 1: Write the failing test**

Create `packages/laravel-queue/tests/Unit/HorizonRabbitMqJobTest.php`:

```php
<?php

declare(strict_types=1);

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqJob as HorizonRabbitMqJob;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Pool;
use Laravel\Horizon\Events\JobDeleted;

function horizonDelivery(int $attempts = 1): Delivery
{
    return new Delivery(
        json_encode([
            'uuid' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
            'job' => 'TestJob',
            'data' => ['report' => 42],
        ], JSON_THROW_ON_ERROR),
        [
            'message_id' => '018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137',
            'subscription' => 'default',
            'attempts' => $attempts,
            'state' => 'pending',
            'headers' => [],
        ],
    );
}

function horizonJob(Delivery $delivery, HorizonRabbitMqQueue $queue): HorizonRabbitMqJob
{
    return new HorizonRabbitMqJob(
        app(),
        $delivery,
        'rabbit-rs',
        'orders.high',
        $queue,
    );
}

function horizonQueue(): HorizonRabbitMqQueue
{
    $queue = new HorizonRabbitMqQueue(new Pool(), [
        'default' => [
            'broker' => 'default-broker',
            'exchange' => 'jobs',
            'routing_key' => '{queue}',
        ],
    ], 'default');
    $queue->setContainer(app());
    $queue->setConnectionName('rabbit-rs');

    return $queue;
}

it('calls deleteReserved on the queue after delete', function (): void {
    $queue = horizonQueue();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $deleteReservedCalled = false;
    // Track via event dispatch
    $events = $this->createMock(\Illuminate\Contracts\Events\Dispatcher::class);
    $events->expects($this->once())
        ->method('dispatch')
        ->willReturnCallback(function (object $event) use (&$deleteReservedCalled): void {
            if ($event instanceof JobDeleted) {
                $deleteReservedCalled = true;
            }
        });
    $this->app->instance(\Illuminate\Contracts\Events\Dispatcher::class, $events);

    $job->delete();

    expect($deleteReservedCalled)->toBeTrue()
        ->and($job->isDeleted())->toBeTrue()
        ->and($delivery->ackCalls)->toBe(1);
});

it('does not call deleteReserved when already deleted', function (): void {
    $queue = horizonQueue();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $job->delete();
    $job->delete();

    expect($delivery->ackCalls)->toBe(1)
        ->and($job->isDeleted())->toBeTrue();
});

it('releases through the native delivery handle', function (): void {
    $queue = horizonQueue();
    $delivery = horizonDelivery();
    $job = horizonJob($delivery, $queue);

    $job->release(5);

    expect([5_000])->toBe($delivery->releaseDelays)
        ->and($job->isReleased())->toBeTrue();
});

it('preserves job id, attempts, and raw body from parent', function (): void {
    $delivery = horizonDelivery(attempts: 3);
    $job = horizonJob($delivery, horizonQueue());

    expect('018f8f1a-5f47-7bc1-9d3b-4ea5a9ce9137')->toBe($job->getJobId())
        ->and(3)->toBe($job->attempts())
        ->and($delivery->payload())->toBe($job->getRawBody())
        ->and('rabbit-rs')->toBe($job->getConnectionName())
        ->and('orders.high')->toBe($job->getQueue());
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/HorizonRabbitMqJobTest.php`
Expected: FAIL — `HorizonRabbitMqJob` class not found

- [ ] **Step 3: Create `Horizon\RabbitMqJob`**

Create `packages/laravel-queue/src/Horizon/RabbitMqJob.php`:

```php
<?php

declare(strict_types=1);

namespace Goopil\RabbitRs\Laravel\Horizon;

use Goopil\RabbitRs\Delivery;
use Goopil\RabbitRs\Laravel\Jobs\RabbitMqJob;
use Illuminate\Container\Container;

class RabbitMqJob extends RabbitMqJob
{
    public function __construct(
        Container $container,
        Delivery $delivery,
        string $connectionName,
        string $queue,
        private readonly RabbitMqQueue $rabbitmq,
    ) {
        parent::__construct($container, $delivery, $connectionName, $queue);
    }

    public function delete(): void
    {
        if ($this->isDeletedOrReleased()) {
            return;
        }

        parent::delete();

        $this->rabbitmq->deleteReserved($this->queue, $this);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/HorizonRabbitMqJobTest.php`
Expected: PASS

- [ ] **Step 5: Run full unit suite to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/src/Horizon/RabbitMqJob.php packages/laravel-queue/tests/Unit/HorizonRabbitMqJobTest.php
git commit -m "feat(horizon): add Horizon\RabbitMqJob with deleteReserved call"
```

---

### Task 5: Wire dynamic class resolution in `RabbitMqConnector`

**Files:**
- Modify: `packages/laravel-queue/src/Connectors/RabbitMqConnector.php:36-63`

**Interfaces:**
- Consumes: `Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue` (from Task 3)
- Consumes: `Goopil\RabbitRs\Laravel\RabbitMqQueue` (existing)
- Produces: `RabbitMqConnector::connect()` returns `Horizon\RabbitMqQueue` when `worker=horizon`, `RabbitMqQueue` otherwise

- [ ] **Step 1: Write the failing test**

Create `packages/laravel-queue/tests/Unit/RabbitMqConnectorHorizonTest.php`:

```php
<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Config\ConfigNormalizer;
use Goopil\RabbitRs\Laravel\Connectors\RabbitMqConnector;
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqQueue;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;
use Goopil\RabbitRs\Laravel\Support\NativePoolFactory;

beforeEach(function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
    ]);

    (new class($this->app) extends RabbitMqServiceProvider {
        protected function nativeExtensionLoaded(): bool
        {
            return true;
        }
    })->boot();
});

it('instantiates HorizonRabbitMqQueue when worker=horizon', function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-horizon', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'horizon',
    ]);

    $queue = $this->app['queue']->connection('rabbit-rs-horizon');

    expect($queue)->toBeInstanceOf(HorizonRabbitMqQueue::class)
        ->and($queue)->toBeInstanceOf(RabbitMqQueue::class);
});

it('instantiates RabbitMqQueue when worker=default', function (): void {
    $this->app['config']->set('queue.connections.rabbit-rs-default', [
        'driver' => 'rabbit-rs',
        'queue' => 'default',
        'worker' => 'default',
    ]);

    $queue = $this->app['queue']->connection('rabbit-rs-default');

    expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
        ->and($queue)->not->toBeInstanceOf(HorizonRabbitMqQueue::class);
});

it('instantiates RabbitMqQueue when worker is not set', function (): void {
    $queue = $this->app['queue']->connection('rabbit-rs');

    expect($queue)->toBeInstanceOf(RabbitMqQueue::class)
        ->and($queue)->not->toBeInstanceOf(HorizonRabbitMqQueue::class);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/RabbitMqConnectorHorizonTest.php`
Expected: FAIL — connector always returns `RabbitMqQueue` regardless of `worker` config

- [ ] **Step 3: Modify `RabbitMqConnector::connect()`**

In `packages/laravel-queue/src/Connectors/RabbitMqConnector.php`, modify the `connect()` method (lines 36-63).

Add import at top:

```php
use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue as HorizonRabbitMqQueue;
```

Replace the body of `connect()` starting at line 54:

```php
        $worker = $config['worker'] ?? 'default';
        $class = $worker === 'horizon'
            ? HorizonRabbitMqQueue::class
            : RabbitMqQueue::class;

        return new $class(
            $this->pools->make($this->normalizedConfig['native']),
            $this->normalizedConfig['routes'],
            $defaultQueue,
            $dispatchAfterCommit,
            workerProfiles: $this->workerProfiles,
            blockForMilliseconds: ($blockFor ?? 0) * 1000,
            publisherConfig: $this->normalizedConfig['publisher'],
        );
```

Also update the return type of `connect()` from `RabbitMqQueue` to `RabbitMqQueue` (it already returns `RabbitMqQueue` — `HorizonRabbitMqQueue` extends it, so the type is compatible).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/RabbitMqConnectorHorizonTest.php`
Expected: PASS

- [ ] **Step 5: Run full unit suite to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS — all existing unit tests still pass (existing tests don't set `worker`, so they get `default`)

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/src/Connectors/RabbitMqConnector.php packages/laravel-queue/tests/Unit/RabbitMqConnectorHorizonTest.php
git commit -m "feat(horizon): wire dynamic class resolution in connector by worker config"
```

---

### Task 6: Add `worker` config key and composer suggest

**Files:**
- Modify: `packages/laravel-queue/config/rabbit-rs.php` (add after line 331, before closing `];`)
- Modify: `packages/laravel-queue/composer.json`

- [ ] **Step 1: Add `worker` key to config**

In `packages/laravel-queue/config/rabbit-rs.php`, add before the closing `];` (after the `topology` block, line 331):

```php

    /*
    |--------------------------------------------------------------------------
    | Worker Mode
    |--------------------------------------------------------------------------
    |
    | Controls which queue worker class is used when the connector resolves
    | a connection.
    |
    | - default: Standard RabbitMqQueue. Use this when Horizon is not
    |           installed or when you don't need Horizon integration.
    |
    | - horizon: HorizonRabbitMqQueue. Dispatches Horizon events
    |           (JobPending, JobPushed, JobReserved, JobDeleted) so Rabbit RS
    |           jobs appear in the Horizon dashboard. Requires laravel/horizon.
    |
    */

    'worker' => env('RABBIT_RS_WORKER', 'default'),
```

- [ ] **Step 2: Add `laravel/horizon` to composer.json suggest**

In `packages/laravel-queue/composer.json`, add a `suggest` key after the `extra` block (before `scripts`):

```json
    "suggest": {
        "laravel/horizon": "Required to use the Horizon dashboard integration (worker=horizon)"
    },
```

- [ ] **Step 3: Run config validation test to verify no regression**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ConfigNormalizerTest.php`
Expected: PASS — `ConfigNormalizer` passes the `worker` key through as part of the per-connection config (it's not part of the native config)

- [ ] **Step 4: Run full unit suite**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue/config/rabbit-rs.php packages/laravel-queue/composer.json
git commit -m "feat(horizon): add worker config key and suggest laravel/horizon"
```

---

### Task 7: Run full quality gate

**Files:** None (verification only)

- [ ] **Step 1: Run the Laravel package test suite**

Run: `cd packages/laravel-queue && php vendor/bin/pest`
Expected: PASS — all unit and feature tests pass

- [ ] **Step 2: Run composer validate**

Run: `cd packages/laravel-queue && rtk composer validate --strict`
Expected: PASS

- [ ] **Step 3: Run the full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS — Rust tests, clippy, fmt, and composer validate all pass

- [ ] **Step 4: Commit any remaining changes**

If there are any uncommitted changes from formatting or fixes:

```bash
git add packages/laravel-queue
git commit -m "chore: quality gate pass for Horizon support"
```
